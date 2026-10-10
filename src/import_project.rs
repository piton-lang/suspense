//! Importing an existing folder as a Piton project: a form like New
//! Project's, the folder chosen in place of a name and location, which writes
//! the folder's piton.config.pi set up for Belay, creates whatever roots
//! aren't there, and opens it, never overwriting, moving, or deleting
//! anything already in the folder, as the ImportProjectScope says.

use crate::process::Logged as _;
use std::path::{Path, PathBuf};

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
use crate::new_project::{
    Agent, GITIGNORE, ProjectCreated, Settings, opencode_json, project_path,
};
use crate::project_directory::{CONFIG_FILE_NAME, ProjectDirectory};
use crate::project_templates;

actions!(suspense, [ImportProject]);

/// Emitted to close the form without importing anything.
pub struct CloseImportProject;

/// The heading the lines Suspense adds to an existing .gitignore go under.
const GITIGNORE_HEADING: &str = "# Suspense";

/// What a folder chosen to import holds, as far as importing it goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FolderState {
    /// It holds a piton.config.pi already: it is a project.
    pub is_project: bool,
    /// The user can write in it.
    pub writable: bool,
    /// The top of the Git repository it is in, whether it is the top or a
    /// folder within it.
    pub repo_top: Option<PathBuf>,
    /// It has a src folder, which the code root starts as.
    pub has_src: bool,
}

impl FolderState {
    /// Reads what `folder` holds. Runs git, so call it off the UI thread.
    pub fn read(folder: &Path) -> Self {
        Self {
            is_project: folder.join(CONFIG_FILE_NAME).exists(),
            writable: writable(folder),
            repo_top: repo_top(folder),
            has_src: folder.join("src").is_dir(),
        }
    }

    /// Why the folder can't be imported, if it can't.
    pub fn problem(&self) -> Option<&'static str> {
        if self.is_project {
            Some("This folder is already a project")
        } else if !self.writable {
            Some("You can't write to this folder")
        } else {
            None
        }
    }
}

/// Whether the user can write in `folder`: whether a file can be made there,
/// tried with one of the application's own, removed again at once.
fn writable(folder: &Path) -> bool {
    if !folder.is_dir() {
        return false;
    }
    let probe = folder.join(format!(".suspense-import-check-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            std::fs::remove_file(&probe).ok();
            true
        }
        Err(err) => err.kind() == std::io::ErrorKind::AlreadyExists,
    }
}

/// The top of the Git repository `folder` is in, as `git rev-parse
/// --show-toplevel` run in it finds; none where it is in none, or git can't
/// be run.
pub fn repo_top(folder: &Path) -> Option<PathBuf> {
    let output = crate::process::command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(folder)
        .output_logged()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let top = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!top.is_empty()).then(|| PathBuf::from(top))
}

/// The code root a folder starts with: ./src where it has a src folder, and
/// ./, the folder itself, where it doesn't.
pub fn default_code_root(has_src: bool) -> &'static str {
    if has_src { "./src" } else { "./" }
}

/// `existing`, a .gitignore's text, with the lines of [`GITIGNORE`] it lacks
/// added at its end, beneath a "# Suspense" heading, the rest kept as it
/// was; none where it lacks none.
pub fn merged_gitignore(existing: &str) -> Option<String> {
    let has: Vec<&str> = existing.lines().map(str::trim).collect();
    let missing: Vec<&str> = GITIGNORE
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .filter(|line| !has.contains(&line.trim()))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let mut merged = existing.to_string();
    if !merged.is_empty() && !merged.ends_with('\n') {
        merged.push('\n');
    }
    if !merged.is_empty() {
        merged.push('\n');
    }
    merged.push_str(GITIGNORE_HEADING);
    merged.push('\n');
    for line in missing {
        merged.push_str(line);
        merged.push('\n');
    }
    Some(merged)
}

/// Imports `folder` with `settings`, its name the folder's: creates the spec,
/// code, and any shape root where they aren't, writes the template's files
/// only where none of that name is, opencode.json where there is none, makes
/// it a Git repository if asked and it isn't in one, gives a repository's
/// .gitignore the lines it lacks, and writes its piton.config.pi last, never
/// overwriting anything. Returns a warning if Git failed.
pub fn import(folder: &Path, settings: &Settings) -> Result<Option<String>> {
    if let Some(problem) = settings.problem() {
        bail!("{problem}");
    }
    let state = FolderState::read(folder);
    if let Some(problem) = state.problem() {
        bail!("{problem}");
    }
    let dir = |path: &str| folder.join(project_path(path).unwrap_or_default());
    let spec_root = dir(&settings.spec_root);
    std::fs::create_dir_all(&spec_root)
        .with_context(|| format!("Couldn't create {}", spec_root.display()))?;
    let templates = project_templates::all();
    let template = templates
        .iter()
        .find(|template| template.key == settings.template)
        .or(templates.first())
        .context("There are no project templates")?;
    // It writes only where no file of that name is.
    template.write(&spec_root)?;
    let mut roots = vec![dir(&settings.code_root)];
    if !settings.shape_root.trim().is_empty() {
        roots.push(dir(&settings.shape_root));
    }
    for root in roots {
        std::fs::create_dir_all(&root)
            .with_context(|| format!("Couldn't create {}", root.display()))?;
    }
    let opencode = folder.join("opencode.json");
    if settings.agents.contains(&Agent::OpenCode) && !opencode.exists() {
        let code_root = project_path(&settings.code_root).unwrap_or_default();
        std::fs::write(&opencode, opencode_json(&code_root))
            .with_context(|| format!("Couldn't write {}", opencode.display()))?;
    }
    crate::agent::set_for_project(folder, settings.harness)?;
    let warning = git(folder, settings.init_git)
        .err()
        .map(|err| format!("Couldn't initialize a Git repository: {err:#}"));

    // Last, so a folder whose import failed is never taken for a project.
    let config = crate::file_view::format_piton(&settings.config(), folder)
        .unwrap_or_else(|_| settings.config());
    let config_file = folder.join(CONFIG_FILE_NAME);
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config_file)
        .and_then(|mut file| file.write_all(config.as_bytes()))
        .with_context(|| format!("Couldn't write {}", config_file.display()))?;
    Ok(warning)
}

/// Makes `folder` a Git repository with `git init` if `init` asks, unless it
/// is in one by now, the top of one or within one, which is never
/// initialized again nor has one nested inside it; then, where it is in a
/// repository, gives its .gitignore the lines it lacks. Nothing is committed.
fn git(folder: &Path, init: bool) -> Result<()> {
    if init && repo_top(folder).is_none() {
        let output = crate::process::command("git")
            .args(["init", "--quiet"])
            .current_dir(folder)
            .output_logged()
            .context("git couldn't be run")?;
        if !output.status.success() {
            bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
        }
    }
    if repo_top(folder).is_none() {
        return Ok(());
    }
    let gitignore = folder.join(".gitignore");
    match std::fs::read_to_string(&gitignore) {
        Ok(existing) => {
            if let Some(merged) = merged_gitignore(&existing) {
                std::fs::write(&gitignore, merged)
                    .with_context(|| format!("Couldn't write {}", gitignore.display()))?;
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&gitignore, GITIGNORE)
                .with_context(|| format!("Couldn't write {}", gitignore.display()))?;
        }
        Err(err) => {
            return Err(err).with_context(|| format!("Couldn't read {}", gitignore.display()));
        }
    }
    Ok(())
}

pub struct ImportProjectForm {
    spec_root: Entity<InputState>,
    code_root: Entity<InputState>,
    shape_root: Entity<InputState>,
    /// The folder chosen, and what it holds, once read.
    folder: Option<PathBuf>,
    state: Option<FolderState>,
    /// The code root last started from a folder, replaced when another is
    /// chosen only while it hasn't been typed over.
    code_root_default: String,
    agents: Vec<Agent>,
    harness: Agent,
    init_git: bool,
    /// The key of the template chosen.
    template: String,
    /// The folder browser, while choosing the folder.
    browser: Option<Entity<FsBrowser>>,
    importing: bool,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    browse_focus: FocusHandle,
    _read: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ProjectCreated> for ImportProjectForm {}
impl EventEmitter<CloseImportProject> for ImportProjectForm {}

impl Focusable for ImportProjectForm {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ImportProjectForm {
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
        let spec_root = input("./spec", "./spec", window, cx);
        let code_root = input("./src", "./src", window, cx);
        let shape_root = input("./spec/shape", "None", window, cx);
        let subscriptions = [&spec_root, &code_root, &shape_root]
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
        let browse_focus = cx.focus_handle();
        // The folder's Browse button has focus as the form opens.
        browse_focus.focus(window, cx);
        Self {
            spec_root,
            code_root,
            shape_root,
            folder: None,
            state: None,
            code_root_default: "./src".into(),
            agents: vec![Agent::Claude],
            harness: Agent::Claude,
            init_git: true,
            template: project_templates::all()
                .first()
                .map(|template| template.key.to_string())
                .unwrap_or_default(),
            browser: None,
            importing: false,
            error: None,
            focus_handle: cx.focus_handle(),
            browse_focus,
            _read: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    /// What the form collects, the project named after the folder.
    pub fn settings(&self, cx: &App) -> Settings {
        let folder = self.folder.clone().unwrap_or_default();
        Settings {
            name: folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            location: folder.parent().map(Path::to_path_buf).unwrap_or_default(),
            agents: self.agents.clone(),
            harness: crate::new_project::harness_among(&self.agents, self.harness),
            spec_root: self.spec_root.read(cx).value().to_string(),
            code_root: self.code_root.read(cx).value().to_string(),
            shape_root: self.shape_root.read(cx).value().to_string(),
            // Never to initialize a repository the folder is in already.
            init_git: self.init_git && self.in_repo().is_none(),
            template: self.template.clone(),
        }
    }

    /// The top of the repository the folder is in, if it is in one.
    fn in_repo(&self) -> Option<&Path> {
        self.state.as_ref()?.repo_top.as_deref()
    }

    /// Why the folder can't be imported yet, if it can't.
    fn problem(&self, cx: &App) -> Option<String> {
        let Some(state) = &self.state else {
            return Some(if self.folder.is_some() {
                "Reading the folder".to_string()
            } else {
                "Choose a folder to import".to_string()
            });
        };
        if let Some(problem) = state.problem() {
            return Some(problem.to_string());
        }
        self.settings(cx).problem()
    }

    /// Makes `folder` the one to import, reading what it holds, from which
    /// the fields start.
    pub fn set_folder(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.folder = Some(folder.clone());
        self.state = None;
        self.error = None;
        let read = cx.background_spawn({
            let folder = folder.clone();
            async move { FolderState::read(&folder) }
        });
        self._read = cx.spawn_in(window, async move |this, cx| {
            let state = read.await;
            this.update_in(cx, |this, window, cx| {
                if this.folder.as_deref() != Some(folder.as_path()) {
                    return;
                }
                // The code root starts from the folder, unless typed over.
                let default = default_code_root(state.has_src).to_string();
                let current = this.code_root.read(cx).value().to_string();
                if current == this.code_root_default && current != default {
                    this.code_root.update(cx, |input, cx| {
                        input.set_value(default.clone(), window, cx)
                    });
                }
                this.code_root_default = default;
                this.state = Some(state);
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn toggle_agent(&mut self, agent: Agent, on: bool, cx: &mut Context<Self>) {
        self.agents.retain(|checked| *checked != agent);
        if on {
            self.agents.push(agent);
        }
        self.harness = crate::new_project::harness_among(&self.agents, self.harness);
        cx.notify();
    }

    /// Opens the folder browser in place of the form.
    pub fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start = ProjectDirectory::get(cx)
            .and_then(|dir| dir.parent().map(Path::to_path_buf))
            .unwrap_or_else(fs_browser::home);
        let browser =
            cx.new(|cx| FsBrowser::folder("Choose the folder to import", start, cx));
        browser.read(cx).focus_handle(cx).focus(window, cx);
        self._subscriptions.push(cx.subscribe_in(
            &browser,
            window,
            |this, _, ChoosePath(folder), window, cx| {
                this.set_folder(folder.clone(), window, cx);
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
        self.browse_focus.focus(window, cx);
        cx.notify();
    }

    /// Imports the folder, then says so, or shows what went wrong.
    pub fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.importing || self.problem(cx).is_some() {
            return;
        }
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let settings = self.settings(cx);
        self.importing = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn({
            let folder = folder.clone();
            async move { import(&folder, &settings) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.importing = false;
                match result {
                    Ok(warning) => cx.emit(ProjectCreated(folder, warning)),
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A labelled root field, with why its value can't be used beneath it,
    /// or, where it names a folder already there, that it uses it.
    fn root_field(
        &self,
        label: &'static str,
        input: &Entity<InputState>,
        required: bool,
        cx: &App,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let value = input.read(cx).value().to_string();
        let problem = if value.trim().is_empty() {
            required.then(|| "Give it a path".to_string())
        } else {
            project_path(&value).err().map(str::to_string)
        };
        let existing = problem.is_none()
            && !value.trim().is_empty()
            && self.folder.as_ref().is_some_and(|folder| {
                project_path(&value).is_ok_and(|path| folder.join(path).is_dir())
            });
        v_flex()
            .gap_1()
            .child(div().text_sm().font_medium().child(label))
            .child(Input::new(input))
            .when_some(problem, |field, problem| {
                field.child(div().text_xs().text_color(theme.danger).child(problem))
            })
            .when(existing, |field| {
                field.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Uses the existing folder"),
                )
            })
    }

    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let problem = self.problem(cx);

        let heading = h_flex()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_1().font_semibold().child("Import Project"))
            .child(
                Button::new("import-project-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close without importing a project")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseImportProject))),
            );

        let folder_problem = self.state.as_ref().and_then(FolderState::problem);
        let is_project = self.state.as_ref().is_some_and(|state| state.is_project);
        let folder = v_flex()
            .gap_1()
            .child(div().text_sm().font_medium().child("Folder"))
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
                            .map(|row| match &self.folder {
                                Some(folder) => row.child(
                                    folder
                                        .file_name()
                                        .map(|name| name.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| folder.display().to_string()),
                                ),
                                None => row
                                    .text_color(theme.muted_foreground)
                                    .child("Choose a folder"),
                            }),
                    )
                    .child(
                        div().track_focus(&self.browse_focus).child(
                            Button::new("import-project-browse")
                                .label("Browse…")
                                .icon(IconName::FolderOpen)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.browse(window, cx)),
                                ),
                        ),
                    ),
            )
            .when_some(self.folder.clone(), |row, folder| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(folder.display().to_string()),
                )
            })
            .when_some(folder_problem, |row, problem| {
                row.child(
                    h_flex()
                        .id("import-project-folder-problem")
                        .gap_2()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(problem)
                        .when(is_project, |row| {
                            row.child(
                                Button::new("import-project-open-instead")
                                    .link()
                                    .xsmall()
                                    .label("Open it instead")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(folder) = this.folder.clone() {
                                            cx.emit(ProjectCreated(folder, None));
                                        }
                                    })),
                            )
                        })
                        .map(gpui_kit::TestSupportExt::test_support),
                )
            });

        let repo = self.in_repo().map(Path::to_path_buf);
        let git = match repo {
            Some(top) => div()
                .id("import-project-git-row")
                .child(
                    checkbox("import-project-git", "Already a Git repository")
                        .checked(false)
                        .disabled(true),
                )
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(format!(
                        "In the Git repository at {}",
                        top.display()
                    ))
                    .build(window, cx)
                })
                .into_any_element(),
            None => div()
                .id("import-project-git-row")
                .child(
                    checkbox("import-project-git", "Initialize a Git repository")
                        .checked(self.init_git)
                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                            this.init_git = *on;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        };

        // Lets UI tests find the row; inert in normal builds.
        let git = gpui_kit::TestSupportExt::test_support(
            div().id("import-project-git-field").child(git),
        );

        let templates =
            v_flex()
                .id("import-project-templates")
                .gap_2()
                .child(div().text_sm().font_medium().child("Template"))
                .children(project_templates::all().into_iter().enumerate().map(
                    |(ix, template)| {
                        let key = template.key;
                        let radio = Radio::new(("import-project-template", ix))
                            .label(template.name.clone())
                            .checked(self.template == key)
                            .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                this.template = key.to_string();
                                cx.notify();
                            }));
                        v_flex().gap_0p5().child(radio).child(
                            div()
                                .pl_6()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(template.description),
                        )
                    },
                ));

        let agents = v_flex()
            .id("import-project-agents")
            .gap_1p5()
            .child(div().text_sm().font_medium().child("Agents"))
            .children(Agent::ALL.into_iter().enumerate().map(|(ix, agent)| {
                let checkbox = checkbox(
                    ElementId::Name(format!("import-project-agent-{ix}").into()),
                    format!("{} ({})", agent.label(), agent.directory()),
                )
                .checked(self.agents.contains(&agent))
                .on_click(
                    cx.listener(move |this, on: &bool, _, cx| this.toggle_agent(agent, *on, cx)),
                );
                // Lets UI tests find the checkbox; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(
                    div().id(("import-project-agent-row", ix)).child(checkbox),
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

        let body = div()
            .id("import-project-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .items_start()
                    .gap_8()
                    .p_6()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_5()
                            .child(folder)
                            .child(git)
                            .child(self.root_field("Spec root", &self.spec_root, true, cx))
                            .child(self.root_field("Code root", &self.code_root, true, cx))
                            .child(self.root_field("Shape root", &self.shape_root, false, cx)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_5()
                            .child(templates)
                            .child(agents)
                            .child(crate::new_project::harness_group(
                                "import-project-harness",
                                &self.agents,
                                self.harness,
                                |this: &mut Self, agent| this.harness = agent,
                                cx,
                            )),
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
                    .id("import-project-error")
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(theme.danger)
                    .children(self.error.clone()),
            )
            .child(
                Button::new("import-project-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseImportProject))),
            )
            .child(
                Button::new("import-project-import")
                    .primary()
                    .label("Import")
                    .loading(self.importing)
                    .disabled(problem.is_some() || self.importing)
                    .tooltip(match (&problem, self.importing) {
                        (_, true) => "Importing the folder".to_string(),
                        (Some(problem), _) => problem.clone(),
                        (None, _) => "Make the folder a project and open it".to_string(),
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.import(window, cx))),
            );

        v_flex()
            .size_full()
            .child(heading)
            .child(body)
            .child(footer)
    }
}

impl Render for ImportProjectForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.browser {
            Some(browser) => browser.clone().into_any_element(),
            None => self.render_form(cx).into_any_element(),
        };
        let form = div()
            .id("import-project")
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

    use super::{FolderState, default_code_root, import, merged_gitignore, repo_top};
    use crate::new_project::{Agent, GITIGNORE, Settings};
    use crate::project_directory::CONFIG_FILE_NAME;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "suspense-import-{name}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn settings(folder: &Path, code_root: &str, init_git: bool) -> Settings {
        Settings {
            name: folder.file_name().unwrap().to_string_lossy().into_owned(),
            location: folder.parent().unwrap().to_path_buf(),
            agents: vec![Agent::Claude, Agent::OpenCode],
            harness: Agent::Claude,
            spec_root: "./spec".into(),
            code_root: code_root.into(),
            shape_root: "./spec/shape".into(),
            init_git,
            template: crate::project_templates::all()[0].key.to_string(),
        }
    }

    fn git(folder: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(folder)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// The code root starts as ./src where there is one, else the folder.
    #[test]
    fn the_code_root_starts_from_what_the_folder_holds() {
        assert_eq!(default_code_root(true), "./src");
        assert_eq!(default_code_root(false), "./");
        let dir = folder("defaults");
        assert!(!FolderState::read(&dir).has_src);
        std::fs::create_dir(dir.join("src")).unwrap();
        let state = FolderState::read(&dir);
        assert!(state.has_src && state.writable && !state.is_project);
        assert_eq!(state.problem(), None);
        std::fs::write(dir.join(CONFIG_FILE_NAME), "").unwrap();
        assert_eq!(
            FolderState::read(&dir).problem(),
            Some("This folder is already a project")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An existing .gitignore keeps what it says, and only the lines it
    /// lacks are added, beneath a heading; one lacking none is left alone.
    #[test]
    fn gitignore_lines_are_added_only_where_missing() {
        let merged = merged_gitignore("target/\n/.suspense/queue/").unwrap();
        assert!(merged.starts_with("target/\n/.suspense/queue/\n\n# Suspense\n"));
        assert!(merged.contains("/.suspense/harness.json\n"));
        assert_eq!(merged.matches("/.suspense/queue/").count(), 1);
        assert_eq!(merged_gitignore(&merged), None);
        assert_eq!(merged_gitignore(GITIGNORE), None);
    }

    /// Importing writes the config last and creates only what isn't there,
    /// keeping every file the folder held, and makes it a repository.
    #[test]
    fn importing_keeps_what_the_folder_holds() {
        let dir = folder("keeps");
        std::fs::write(dir.join("main.py"), "print('hi')\n").unwrap();
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::write(dir.join("spec/index.pi"), "# mine\n").unwrap();
        std::fs::write(dir.join("opencode.json"), "{}\n").unwrap();

        let warning = import(&dir, &settings(&dir, "./", true)).unwrap();
        assert_eq!(warning, None);
        assert_eq!(std::fs::read_to_string(dir.join("main.py")).unwrap(), "print('hi')\n");
        assert_eq!(std::fs::read_to_string(dir.join("spec/index.pi")).unwrap(), "# mine\n");
        assert_eq!(std::fs::read_to_string(dir.join("opencode.json")).unwrap(), "{}\n");
        assert!(dir.join("spec/shape").is_dir());
        let config = std::fs::read_to_string(dir.join(CONFIG_FILE_NAME)).unwrap();
        assert!(config.contains("codeRoot: ."), "{config}");
        assert!(dir.join(".git").is_dir());
        assert_eq!(std::fs::read_to_string(dir.join(".gitignore")).unwrap(), GITIGNORE);
        // Imported, it is a project, which can't be imported again.
        assert!(import(&dir, &settings(&dir, "./", true)).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A folder within a repository is never initialized again, nor given one
    /// nested in it, whatever is asked; its .gitignore gains what it lacks.
    #[test]
    fn a_folder_in_a_repository_is_never_initialized_again() {
        let top = folder("in-repo");
        git(&top, &["init", "--quiet"]);
        let inner = top.join("app");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(inner.join(".gitignore"), "build/\n").unwrap();
        assert_eq!(
            repo_top(&inner).map(|path| std::fs::canonicalize(path).unwrap()),
            Some(std::fs::canonicalize(&top).unwrap())
        );
        import(&inner, &settings(&inner, "./src", true)).unwrap();
        assert!(!inner.join(".git").exists(), "a repository was nested inside");
        let ignore = std::fs::read_to_string(inner.join(".gitignore")).unwrap();
        assert!(ignore.starts_with("build/\n\n# Suspense\n"), "{ignore}");
        assert!(inner.join("src").is_dir());
        std::fs::remove_dir_all(&top).ok();
    }

    /// Left out of Git, and in no repository, nothing Git is written.
    #[test]
    fn without_git_no_repository_or_gitignore_is_made() {
        let dir = folder("no-git");
        // Not inside this repository's own, from the temp folder.
        if repo_top(&dir).is_some() {
            return;
        }
        import(&dir, &settings(&dir, "./src", false)).unwrap();
        assert!(!dir.join(".git").exists());
        assert!(!dir.join(".gitignore").exists());
        assert!(dir.join(CONFIG_FILE_NAME).exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
