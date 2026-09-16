//! Creating a new Piton project: a form for its name, where it goes, the
//! agents Belay writes for, and its spec, code, and shape roots, which writes
//! a piton.config.pi set up for Belay, creates the project's folders, and opens
//! it.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::radio::Radio;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::checkbox::checkbox;
use crate::fs_browser::{self, CancelBrowse, ChoosePath, FsBrowser};
use crate::project_directory::{CONFIG_FILE_NAME, ProjectDirectory};
use crate::project_templates;

actions!(suspense, [NewProject]);

/// Emitted once the project is created, with its folder, and a warning when
/// something that doesn't stop it opening, like Git, failed.
pub struct ProjectCreated(pub PathBuf, pub Option<String>);

/// The .gitignore written into a new repository: what Suspense keeps for this
/// machine alone rather than for the project.
const GITIGNORE: &str = "\
# Suspense's per-machine state. The prompt history is kept.
/.suspense/harness.json
/.suspense/queue/
/.suspense/commit-notes.json
.suspense-draft.pi
";

/// Emitted to close the form without creating anything.
pub struct CloseNewProject;

/// An agent Belay can write for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    OpenCode,
    Codex,
    Cursor,
    Agents,
}

impl Agent {
    /// Every agent, in the order they are listed and written.
    pub const ALL: [Agent; 5] = [
        Agent::Claude,
        Agent::OpenCode,
        Agent::Codex,
        Agent::Cursor,
        Agent::Agents,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::OpenCode => "OpenCode",
            Agent::Codex => "Codex",
            Agent::Cursor => "Cursor",
            Agent::Agents => "Agents",
        }
    }

    /// The directory it writes into.
    pub fn directory(self) -> &'static str {
        match self {
            Agent::Claude => ".claude",
            Agent::OpenCode => ".opencode",
            Agent::Codex => ".codex",
            Agent::Cursor => ".cursor",
            Agent::Agents => ".agents",
        }
    }

    /// Its adapter anchor's name.
    fn adapter(self) -> &'static str {
        match self {
            Agent::Claude => "ClaudeAdapter",
            Agent::OpenCode => "OpenCodeAdapter",
            Agent::Codex => "CodexAdapter",
            Agent::Cursor => "CursorAdapter",
            Agent::Agents => "AgentsAdapter",
        }
    }

    /// The tool flag its adapter sets.
    fn flag(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::OpenCode => "opencode",
            Agent::Codex => "codex",
            Agent::Cursor => "cursor",
            Agent::Agents => "agents",
        }
    }

    /// Whether @piton/belay exports its adapter ready-made.
    fn ready_made(self) -> bool {
        matches!(self, Agent::Claude | Agent::OpenCode)
    }
}

/// Everything the form collects.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub name: String,
    pub location: PathBuf,
    pub agents: Vec<Agent>,
    pub spec_root: String,
    pub code_root: String,
    /// Empty to go without one.
    pub shape_root: String,
    pub init_git: bool,
    /// The key of the template the project starts from.
    pub template: String,
}

/// Why a name can't be the project's folder, if it can't.
pub fn name_problem(name: &str) -> Option<&'static str> {
    let name = name.trim();
    if name.is_empty() {
        Some("Give the project a name")
    } else if name.contains(['/', '\\']) {
        Some("A name can't contain a slash")
    } else if name == "." || name == ".." {
        Some("A name can't be . or ..")
    } else {
        None
    }
}

/// `path` as written in the config, with a leading ./, or why it can't be
/// used: it is absolute, or climbs out of the project.
pub fn project_path(path: &str) -> Result<String, &'static str> {
    let path = path.trim();
    let parsed = Path::new(path);
    if parsed.is_absolute() {
        return Err("Use a path inside the project, not an absolute one");
    }
    if parsed
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("A path can't climb out of the project with ..");
    }
    let rest = path.trim_start_matches("./").trim_end_matches('/');
    if rest.is_empty() || rest == "." {
        return Ok(".".to_string());
    }
    Ok(format!("./{rest}"))
}

/// The project's anchor name: its name in PascalCase from its letters and
/// digits, or "Project" when that leaves nothing or starts with a digit.
pub fn anchor_name(name: &str) -> String {
    let pascal: String = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect();
    match pascal.chars().next() {
        Some(first) if first.is_ascii_alphabetic() => pascal,
        _ => "Project".to_string(),
    }
}

impl Settings {
    /// The folder the project is created in.
    pub fn folder(&self) -> PathBuf {
        self.location.join(self.name.trim())
    }

    /// Why the project can't be created yet, if it can't.
    pub fn problem(&self) -> Option<String> {
        if let Some(problem) = name_problem(&self.name) {
            return Some(problem.to_string());
        }
        for (label, path, required) in [
            ("spec root", &self.spec_root, true),
            ("code root", &self.code_root, true),
            ("shape root", &self.shape_root, false),
        ] {
            if path.trim().is_empty() {
                if required {
                    return Some(format!("Give the {label} a path"));
                }
                continue;
            }
            if let Err(problem) = project_path(path) {
                return Some(format!("The {label}: {problem}"));
            }
        }
        if self.agents.is_empty() {
            return Some("Choose at least one agent".to_string());
        }
        None
    }

    /// The piton.config.pi for the project.
    pub fn config(&self) -> String {
        let path = |path: &str| project_path(path).unwrap_or_else(|_| path.trim().to_string());
        let spec_root = path(&self.spec_root);
        let entry = if spec_root == "." {
            "./index.pi".to_string()
        } else {
            format!("{spec_root}/index.pi")
        };
        let agents: Vec<Agent> = Agent::ALL
            .into_iter()
            .filter(|agent| self.agents.contains(agent))
            .collect();
        let imported: Vec<&str> = agents
            .iter()
            .filter(|agent| agent.ready_made())
            .map(|agent| agent.adapter())
            .collect();

        let mut config = String::from("use @piton/config\nuse @piton/belay\n\n");
        if !imported.is_empty() {
            config += &format!("from @piton/belay import {}\n\n", imported.join(", "));
        }
        config += &format!(
            "export piton-config {}:\n    root: {spec_root}\n    entry: {entry}\n\n    frameworks:\n        - {{BelayConfiguration}}\n\n",
            anchor_name(&self.name)
        );
        config += &format!(
            "belay-config BelayConfiguration:\n    codeRoot: {}\n",
            path(&self.code_root)
        );
        if !self.shape_root.trim().is_empty() {
            config += &format!("    shapeRoot: {}\n", path(&self.shape_root));
        }
        config += "\n    adapters:\n";
        for agent in &agents {
            config += &format!("        - {{{}}}\n", agent.adapter());
        }
        for agent in agents.iter().filter(|agent| !agent.ready_made()) {
            config += &format!(
                "\nbelay-agent-adapter {}:\n    description: Writes agentic Markdown into the {} directory\n    {}: true\n",
                agent.adapter(),
                agent.directory(),
                agent.flag()
            );
        }
        config
    }

    /// Creates the project: its folder, piton.config.pi, the spec root with an
    /// empty index.pi, and the code and any shape root, never overwriting
    /// anything; then, if asked, makes it a Git repository. Returns the
    /// project's folder, and a warning if Git failed.
    pub fn create(&self) -> Result<(PathBuf, Option<String>)> {
        if let Some(problem) = self.problem() {
            bail!("{problem}");
        }
        let folder = self.folder();
        let config_file = folder.join(CONFIG_FILE_NAME);
        if config_file.exists() {
            bail!("{} already holds a {CONFIG_FILE_NAME}", folder.display());
        }
        std::fs::create_dir_all(&folder)
            .with_context(|| format!("Couldn't create {}", folder.display()))?;
        let config = crate::file_view::format_piton(&self.config(), &folder)
            .unwrap_or_else(|_| self.config());
        std::fs::write(&config_file, config)
            .with_context(|| format!("Couldn't write {}", config_file.display()))?;

        let dir = |path: &str| folder.join(project_path(path).unwrap_or_default());
        let spec_root = dir(&self.spec_root);
        std::fs::create_dir_all(&spec_root)
            .with_context(|| format!("Couldn't create {}", spec_root.display()))?;
        let templates = project_templates::all();
        let template = templates
            .iter()
            .find(|template| template.key == self.template)
            .or(templates.first())
            .context("There are no project templates")?;
        template.write(&spec_root)?;
        let mut roots = vec![dir(&self.code_root)];
        if !self.shape_root.trim().is_empty() {
            roots.push(dir(&self.shape_root));
        }
        for root in roots {
            std::fs::create_dir_all(&root)
                .with_context(|| format!("Couldn't create {}", root.display()))?;
        }
        let warning = self
            .init_git
            .then(|| init_git(&folder).err())
            .flatten()
            .map(|err| format!("Couldn't initialize a Git repository: {err:#}"));
        Ok((folder, warning))
    }
}

/// Makes `folder` a Git repository, unless it already is the top of one, and
/// writes the .gitignore unless there is one. Nothing is committed.
pub fn init_git(folder: &Path) -> Result<()> {
    if !folder.join(".git").exists() {
        let output = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(folder)
            .output()
            .context("git couldn't be run")?;
        if !output.status.success() {
            bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
        }
    }
    let gitignore = folder.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, GITIGNORE)
            .with_context(|| format!("Couldn't write {}", gitignore.display()))?;
    }
    Ok(())
}

pub struct NewProjectForm {
    name: Entity<InputState>,
    spec_root: Entity<InputState>,
    code_root: Entity<InputState>,
    shape_root: Entity<InputState>,
    location: PathBuf,
    agents: Vec<Agent>,
    init_git: bool,
    /// The key of the template chosen.
    template: String,
    /// The folder browser, while choosing where the project goes.
    browser: Option<Entity<FsBrowser>>,
    creating: bool,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ProjectCreated> for NewProjectForm {}
impl EventEmitter<CloseNewProject> for NewProjectForm {}

impl Focusable for NewProjectForm {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl NewProjectForm {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input =
            |value: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
                let (value, placeholder) = (value.to_string(), placeholder.to_string());
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(value)
                        .placeholder(placeholder)
                })
            };
        let name = input("", "my-project", window, cx);
        let spec_root = input("./spec", "./spec", window, cx);
        let code_root = input("./src", "./src", window, cx);
        let shape_root = input("./spec/shape", "None", window, cx);
        let subscriptions = [&name, &spec_root, &code_root, &shape_root]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.error = None;
                        cx.notify();
                    }
                })
            })
            .collect();
        let location = ProjectDirectory::get(cx)
            .and_then(|dir| dir.parent().map(Path::to_path_buf))
            .unwrap_or_else(fs_browser::home);
        name.update(cx, |name, cx| name.focus(window, cx));
        Self {
            name,
            spec_root,
            code_root,
            shape_root,
            location,
            agents: vec![Agent::Claude],
            init_git: true,
            template: project_templates::all()
                .first()
                .map(|template| template.key.to_string())
                .unwrap_or_default(),
            browser: None,
            creating: false,
            error: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    pub fn settings(&self, cx: &App) -> Settings {
        Settings {
            name: self.name.read(cx).value().to_string(),
            location: self.location.clone(),
            agents: self.agents.clone(),
            spec_root: self.spec_root.read(cx).value().to_string(),
            code_root: self.code_root.read(cx).value().to_string(),
            shape_root: self.shape_root.read(cx).value().to_string(),
            init_git: self.init_git,
            template: self.template.clone(),
        }
    }

    #[cfg(test)]
    pub fn set_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |input, cx| {
            input.set_value(name.to_string(), window, cx)
        });
    }

    #[cfg(test)]
    pub fn browser(&self) -> Option<Entity<FsBrowser>> {
        self.browser.clone()
    }

    pub fn set_location(&mut self, location: PathBuf, cx: &mut Context<Self>) {
        self.location = location;
        self.error = None;
        cx.notify();
    }

    fn toggle_agent(&mut self, agent: Agent, on: bool, cx: &mut Context<Self>) {
        self.agents.retain(|checked| *checked != agent);
        if on {
            self.agents.push(agent);
        }
        cx.notify();
    }

    /// Opens the folder browser in place of the form.
    pub fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let location = self.location.clone();
        let browser = cx.new(|cx| FsBrowser::folder("Choose where the project goes", location, cx));
        // The browser has the keyboard as it opens.
        browser.read(cx).focus_handle(cx).focus(window, cx);
        self._subscriptions.push(cx.subscribe_in(
            &browser,
            window,
            |this, _, ChoosePath(folder), window, cx| {
                this.set_location(folder.clone(), cx);
                this.close_browser(window, cx);
            },
        ));
        self._subscriptions.push(cx.subscribe_in(
            &browser,
            window,
            |this, _, _: &CancelBrowse, window, cx| this.close_browser(window, cx),
        ));
        self.browser = Some(browser);
        cx.notify();
    }

    fn close_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser = None;
        self.name.update(cx, |name, cx| name.focus(window, cx));
        cx.notify();
    }

    /// Creates the project, then says so, or shows what went wrong.
    pub fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.settings(cx);
        if self.creating || settings.problem().is_some() {
            return;
        }
        self.creating = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move { settings.create() });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.creating = false;
                match result {
                    Ok((folder, warning)) => cx.emit(ProjectCreated(folder, warning)),
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A labelled field, with why its value can't be used beneath it once
    /// something has been typed.
    fn field(
        label: &'static str,
        input: &Entity<InputState>,
        problem: Option<String>,
        cx: &App,
    ) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .gap_1()
            .child(div().text_sm().font_medium().child(label))
            .child(Input::new(input))
            .when_some(problem, |field, problem| {
                field.child(div().text_xs().text_color(theme.danger).child(problem))
            })
    }

    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let settings = self.settings(cx);
        let problem = settings.problem();
        let typed = |input: &Entity<InputState>| !input.read(cx).value().is_empty();
        let name_problem = name_problem(&settings.name)
            .filter(|_| typed(&self.name))
            .map(str::to_string);
        let path_problem = |input: &Entity<InputState>, required: bool| {
            let value = input.read(cx).value();
            if value.trim().is_empty() {
                return required.then(|| "Give it a path".to_string());
            }
            project_path(&value).err().map(str::to_string)
        };

        let heading = h_flex()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_1().font_semibold().child("New Project"))
            .child(
                Button::new("new-project-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close without creating a project")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseNewProject))),
            );

        let location = v_flex()
            .gap_1()
            .child(div().text_sm().font_medium().child("Location"))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .child(self.location.display().to_string()),
                    )
                    .child(
                        Button::new("new-project-browse")
                            .label("Browse…")
                            .icon(IconName::FolderOpen)
                            .on_click(cx.listener(|this, _, window, cx| this.browse(window, cx))),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("Creates {}", settings.folder().display())),
            );

        let git = checkbox("new-project-git", "Initialize a Git repository")
            .checked(self.init_git)
            .on_click(cx.listener(|this, on: &bool, _, cx| {
                this.init_git = *on;
                cx.notify();
            }));

        let templates =
            v_flex()
                .id("new-project-templates")
                .gap_2()
                .child(div().text_sm().font_medium().child("Template"))
                .children(project_templates::all().into_iter().enumerate().map(
                    |(ix, template)| {
                        let key = template.key;
                        let radio = Radio::new(("new-project-template", ix))
                            .label(template.name.clone())
                            .checked(self.template == key)
                            .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                this.template = key.to_string();
                                cx.notify();
                            }));
                        let row = v_flex()
                            .id(("new-project-template-row", ix))
                            .gap_0p5()
                            .child(radio)
                            .child(
                                div()
                                    .pl_6()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(template.description),
                            );
                        // Lets UI tests find the row; inert in normal builds.
                        gpui_kit::TestSupportExt::test_support(row)
                    },
                ));
        // Lets UI tests find the group; inert in normal builds.
        let templates = gpui_kit::TestSupportExt::test_support(templates);

        let agents = v_flex()
            .id("new-project-agents")
            .gap_1p5()
            .child(div().text_sm().font_medium().child("Agents"))
            .children(Agent::ALL.into_iter().enumerate().map(|(ix, agent)| {
                let checkbox = checkbox(
                    ElementId::Name(format!("new-project-agent-{ix}").into()),
                    format!("{} ({})", agent.label(), agent.directory()),
                )
                .checked(self.agents.contains(&agent))
                .on_click(
                    cx.listener(move |this, on: &bool, _, cx| this.toggle_agent(agent, *on, cx)),
                );
                // Lets UI tests find the checkbox; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(
                    div().id(("new-project-agent-row", ix)).child(checkbox),
                )
            }))
            .when(self.agents.is_empty(), |agents| {
                agents.child(
                    div()
                        .text_xs()
                        .text_color(theme.danger)
                        .child("Choose at least one agent"),
                )
            });
        // Lets UI tests find the group; inert in normal builds.
        let agents = gpui_kit::TestSupportExt::test_support(agents);

        let body = div()
            .id("new-project-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                // The project itself on the left; what it starts from and the
                // agents it writes for on the right, each column top aligned.
                h_flex()
                    .items_start()
                    .gap_8()
                    .p_6()
                    .child(
                        v_flex()
                            .id("new-project-main-column")
                            .flex_1()
                            .min_w_0()
                            .gap_5()
                            .child(Self::field("Name", &self.name, name_problem, cx))
                            .child(location)
                            .child(git)
                            .child(Self::field(
                                "Spec root",
                                &self.spec_root,
                                path_problem(&self.spec_root, true),
                                cx,
                            ))
                            .child(Self::field(
                                "Code root",
                                &self.code_root,
                                path_problem(&self.code_root, true),
                                cx,
                            ))
                            .child(Self::field(
                                "Shape root",
                                &self.shape_root,
                                path_problem(&self.shape_root, false),
                                cx,
                            ))
                            .map(gpui_kit::TestSupportExt::test_support),
                    )
                    .child(
                        v_flex()
                            .id("new-project-side-column")
                            .flex_1()
                            .min_w_0()
                            .gap_5()
                            .child(templates)
                            .child(agents)
                            .map(gpui_kit::TestSupportExt::test_support),
                    ),
            );

        let theme = cx.theme();
        let footer = h_flex()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("new-project-error")
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(theme.danger)
                    .children(self.error.clone()),
            )
            .child(
                Button::new("new-project-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseNewProject))),
            )
            .child(
                Button::new("new-project-create")
                    .primary()
                    .label("New")
                    .loading(self.creating)
                    .disabled(problem.is_some() || self.creating)
                    .tooltip(match (&problem, self.creating) {
                        (_, true) => "Creating the project".to_string(),
                        (Some(problem), _) => problem.clone(),
                        (None, _) => "Create the project and open it".to_string(),
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
            );

        v_flex()
            .size_full()
            .child(heading)
            .child(body)
            .child(footer)
    }
}

impl Render for NewProjectForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.browser {
            Some(browser) => browser.clone().into_any_element(),
            None => self.render_form(cx).into_any_element(),
        };
        let form = div()
            .id("new-project")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .child(content);
        // Lets UI tests find the form; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(form)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{Agent, Settings, anchor_name, name_problem, project_path};

    fn settings(agents: Vec<Agent>) -> Settings {
        Settings {
            name: "my new project".into(),
            location: PathBuf::from("/tmp"),
            agents,
            spec_root: "spec".into(),
            code_root: "./src/".into(),
            shape_root: "./spec/shape".into(),
            init_git: false,
            template: "base".into(),
        }
    }

    #[test]
    fn names_and_paths_are_checked() {
        assert!(name_problem("  ").is_some());
        assert!(name_problem("a/b").is_some());
        assert!(name_problem("..").is_some());
        assert_eq!(name_problem("suspense"), None);

        assert_eq!(project_path("spec"), Ok("./spec".into()));
        assert_eq!(project_path("./src/"), Ok("./src".into()));
        assert!(project_path("/abs").is_err());
        assert!(project_path("../out").is_err());

        assert_eq!(anchor_name("my new-project 2"), "MyNewProject2");
        assert_eq!(anchor_name("2fast"), "Project");
        assert_eq!(anchor_name("???"), "Project");

        let mut no_agents = settings(vec![]);
        assert_eq!(
            no_agents.problem().as_deref(),
            Some("Choose at least one agent")
        );
        no_agents.agents.push(Agent::Codex);
        assert_eq!(no_agents.problem(), None);
    }

    /// The config names the project, points at its roots, and lists an adapter
    /// per agent, importing the ready-made ones and writing out the others.
    #[test]
    fn config_sets_up_belay_for_the_chosen_agents() {
        let config = settings(vec![Agent::Codex, Agent::Claude]).config();
        assert_eq!(
            config,
            "use @piton/config
use @piton/belay

from @piton/belay import ClaudeAdapter

export piton-config MyNewProject:
    root: ./spec
    entry: ./spec/index.pi

    frameworks:
        - {BelayConfiguration}

belay-config BelayConfiguration:
    codeRoot: ./src
    shapeRoot: ./spec/shape

    adapters:
        - {ClaudeAdapter}
        - {CodexAdapter}

belay-agent-adapter CodexAdapter:
    description: Writes agentic Markdown into the .codex directory
    codex: true
"
        );
        let mut without_shape = settings(vec![Agent::Claude]);
        without_shape.shape_root = " ".into();
        assert!(!without_shape.config().contains("shapeRoot"));
    }

    /// Creating writes a config Piton builds, the spec root's index.pi, and
    /// the roots; it refuses a folder that already holds a config.
    #[test]
    fn creating_writes_a_project_piton_builds() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/new-project-test");
        std::fs::remove_dir_all(&base).ok();
        std::fs::create_dir_all(&base).unwrap();
        let mut settings = settings(Agent::ALL.to_vec());
        settings.location = base.clone();

        let (folder, warning) = settings.create().unwrap();
        assert_eq!(warning, None);
        assert!(!folder.join(".git").exists(), "Git was initialized unasked");
        assert_eq!(folder, base.join("my new project"));
        for path in ["piton.config.pi", "spec/index.pi", "src", "spec/shape"] {
            assert!(folder.join(path).exists(), "{path} was not created");
        }
        let build = Command::new("piton")
            .arg("build")
            .current_dir(&folder)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "piton build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let err = settings.create().unwrap_err();
        std::fs::remove_dir_all(&folder).ok();

        // Started from scope, concept, and shape, it's seeded with the lib and
        // an example, and builds all the same; a file already there is kept.
        settings.template = "scope-concept-shape".into();
        std::fs::create_dir_all(folder.join("spec")).unwrap();
        std::fs::write(folder.join("spec/index.pi"), "// mine\n").unwrap();
        let (folder, _) = settings.create().unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.join("spec/index.pi")).unwrap(),
            "// mine\n"
        );
        std::fs::remove_file(folder.join("spec/index.pi")).unwrap();
        std::fs::remove_file(folder.join("piton.config.pi")).unwrap();
        let (folder, _) = settings.create().unwrap();
        for path in [
            "spec/index.pi",
            "spec/lib/index.pi",
            "spec/lib/Scope.pi",
            "spec/lib/Concept.pi",
            "spec/lib/Shape.pi",
            "spec/scope/application/index.pi",
        ] {
            assert!(folder.join(path).exists(), "{path} was not written");
        }
        let build = Command::new("piton")
            .arg("build")
            .current_dir(&folder)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "piton build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        std::fs::remove_dir_all(&folder).ok();
        settings.template = "base".into();

        // Asked for, the folder becomes a repository, ignoring Suspense's
        // per-machine state, with nothing committed.
        settings.init_git = true;
        let (folder, warning) = settings.create().unwrap();
        assert_eq!(warning, None);
        assert!(folder.join(".git").is_dir(), "no repository");
        let ignore = std::fs::read_to_string(folder.join(".gitignore")).unwrap();
        for ignored in [
            "/.suspense/commit-notes.json",
            "/.suspense/queue/",
            "/.suspense/harness.json",
        ] {
            assert!(
                ignore.contains(ignored),
                "{ignored} isn't ignored: {ignore}"
            );
        }
        let log = Command::new("git")
            .args(["rev-list", "--all"])
            .current_dir(&folder)
            .output()
            .unwrap();
        assert!(log.stdout.is_empty(), "something was committed");

        // Again over an existing repository and .gitignore, both are kept.
        std::fs::write(folder.join(".gitignore"), "mine\n").unwrap();
        super::init_git(&folder).unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.join(".gitignore")).unwrap(),
            "mine\n"
        );
        assert!(format!("{err}").contains("already holds"), "{err}");
        std::fs::remove_dir_all(&base).ok();
    }
}
