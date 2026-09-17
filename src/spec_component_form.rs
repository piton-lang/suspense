//! The form that creates a scope, a concept, or a shape (see the
//! SpecComponentsScope), shown in the inset panel: the component's own
//! fields, a description, and along the bottom Cancel, a skill to run, Run
//! Skill, and Create.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IndexPath, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::growing_input::GrowToFit;
use crate::harness_mentions::{self, MentionKind};
use crate::spec_components::{self, ComponentKind, Request, SCOPE_KEYWORDS};

/// Emitted to close the form without creating anything.
pub struct CloseSpecComponent;

/// Emitted once the component is written, with the file written.
pub struct ComponentCreated(pub PathBuf);

/// Emitted to hand the component to a skill, with the prompt to send.
pub struct RunComponentSkill(pub String);

/// The most rows the description grows to.
const DESCRIPTION_MAX_ROWS: usize = 8;

/// What a concept's Shape dropdown offers for no shape.
const NO_SHAPE: &str = "None";

type StringSelect = Entity<SelectState<SearchableVec<String>>>;

pub struct SpecComponentForm {
    kind: ComponentKind,
    /// The spec location's folder.
    spec_dir: PathBuf,
    name: Entity<InputState>,
    /// A scope's keyword.
    keyword: Entity<SelectState<SearchableVec<&'static str>>>,
    /// A scope's folder, following the name until it is edited.
    folder: Entity<InputState>,
    folder_edited: bool,
    /// Set while the folder is filled in from the name, so that isn't taken
    /// for editing it.
    filling_folder: bool,
    /// The spec file a concept or shape goes in.
    file: StringSelect,
    /// The names declared in the chosen file.
    declared: Vec<(String, String)>,
    /// A concept's shape.
    shape: StringSelect,
    description: Entity<EditorState>,
    description_fit: GrowToFit,
    skill: StringSelect,
    creating: bool,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CloseSpecComponent> for SpecComponentForm {}
impl EventEmitter<ComponentCreated> for SpecComponentForm {}
impl EventEmitter<RunComponentSkill> for SpecComponentForm {}

impl Focusable for SpecComponentForm {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SpecComponentForm {
    /// A form for `kind` in the project at `project_dir`, starting on
    /// `open_file` when it is a spec file.
    pub fn new(
        kind: ComponentKind,
        project_dir: PathBuf,
        open_file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (spec_dir, files) = spec_components::spec_files(&project_dir)
            .unwrap_or_else(|_| (project_dir.join("spec"), Vec::new()));
        let chosen = open_file
            .as_deref()
            .and_then(|file| file.strip_prefix(&spec_dir).ok())
            .map(|file| file.to_string_lossy().replace('\\', "/"))
            .and_then(|file| files.iter().position(|f| *f == file));

        let name = cx.new(|cx| {
            InputState::new(window, cx).placeholder(match kind {
                ComponentKind::Scope => "ParserScope",
                ComponentKind::Concept => "ParserConcept",
                ComponentKind::Shape => "ParserShape",
            })
        });
        let keyword = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(SCOPE_KEYWORDS.to_vec()),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        });
        let folder = cx.new(|cx| InputState::new(window, cx).placeholder("scope/parser"));
        let file = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(files),
                chosen.map(IndexPath::new),
                window,
                cx,
            )
            .searchable(true)
        });
        let shape = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(vec![NO_SHAPE.to_string()]),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        });
        let description = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .soft_wrap(true)
                .scroll_beyond_last_line(Some(0))
                .placeholder("Describe what it is for, in plain words")
        });
        let skill = cx.new(|cx| {
            SelectState::new(SearchableVec::new(Vec::<String>::new()), None, window, cx)
                .searchable(true)
        });

        let subscriptions = vec![
            cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.error = None;
                    this.follow_name(window, cx);
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &keyword,
                window,
                |this, _, _: &SelectEvent<SearchableVec<&'static str>>, window, cx| {
                    this.follow_name(window, cx);
                    cx.notify();
                },
            ),
            cx.subscribe(&folder, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    if !this.filling_folder {
                        this.folder_edited = true;
                    }
                    this.error = None;
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &file,
                window,
                |this, _, _: &SelectEvent<SearchableVec<String>>, window, cx| {
                    this.read_file(window, cx);
                    cx.notify();
                },
            ),
            cx.subscribe(&description, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(
                &skill,
                |_, _, _: &SelectEvent<SearchableVec<String>>, cx| cx.notify(),
            ),
        ];

        // The harness's skills, found without holding up the form.
        let found = cx.background_spawn({
            let project_dir = project_dir.clone();
            async move {
                let mut skills: Vec<String> = harness_mentions::discover(Some(&project_dir))
                    .into_iter()
                    .filter(|found| found.kind == MentionKind::Skill)
                    .map(|found| found.name)
                    .collect();
                skills.sort();
                skills
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let skills = found.await;
            this.update_in(cx, |this, window, cx| {
                this.skill.update(cx, |select, cx| {
                    select.set_items(SearchableVec::new(skills), window, cx);
                    select.set_selected_index(None, window, cx);
                });
                cx.notify();
            })
            .ok();
        })
        .detach();

        name.update(cx, |name, cx| name.focus(window, cx));
        let mut this = Self {
            kind,
            spec_dir,
            name,
            keyword,
            folder,
            folder_edited: false,
            filling_folder: false,
            file,
            declared: Vec::new(),
            shape,
            description,
            description_fit: GrowToFit::new(DESCRIPTION_MAX_ROWS),
            skill,
            creating: false,
            error: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.read_file(window, cx);
        this
    }

    pub fn kind(&self) -> ComponentKind {
        self.kind
    }

    fn full_name(&self, cx: &App) -> String {
        spec_components::full_name(&self.name.read(cx).value(), self.kind)
    }

    fn keyword_value(&self, cx: &App) -> &'static str {
        self.keyword
            .read(cx)
            .selected_value()
            .copied()
            .unwrap_or(SCOPE_KEYWORDS[0])
    }

    fn file_value(&self, cx: &App) -> Option<String> {
        self.file.read(cx).selected_value().cloned()
    }

    /// Fills in the folder from the name and keyword, until it is edited.
    fn follow_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind != ComponentKind::Scope || self.folder_edited {
            return;
        }
        let folder = spec_components::default_folder(&self.full_name(cx), self.keyword_value(cx));
        self.filling_folder = true;
        self.folder
            .update(cx, |input, cx| input.set_value(folder, window, cx));
        self.filling_folder = false;
    }

    /// Reads the names declared in the chosen file, and offers its shapes.
    fn read_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.declared = self
            .file_value(cx)
            .and_then(|file| std::fs::read_to_string(self.spec_dir.join(file)).ok())
            .map(|text| spec_components::declared_names(&text))
            .unwrap_or_default();
        let shapes: Vec<String> = std::iter::once(NO_SHAPE.to_string())
            .chain(
                self.declared
                    .iter()
                    .filter(|(keyword, _)| keyword == "shape")
                    .map(|(_, name)| name.clone()),
            )
            .collect();
        self.shape.update(cx, |select, cx| {
            select.set_items(SearchableVec::new(shapes), window, cx);
            select.set_selected_index(Some(IndexPath::new(0)), window, cx);
        });
    }

    /// The form as filled in.
    pub fn request(&self, cx: &App) -> Request {
        Request {
            kind: self.kind,
            name: self.full_name(cx),
            keyword: self.keyword_value(cx).to_string(),
            folder: self.folder.read(cx).value().trim().to_string(),
            file: self.file_value(cx).unwrap_or_default(),
            shape: self
                .shape
                .read(cx)
                .selected_value()
                .filter(|shape| *shape != NO_SHAPE)
                .cloned(),
            description: self.description.read(cx).value().to_string(),
        }
    }

    /// Why the fields can't be used yet, if they can't.
    fn name_problem(&self, cx: &App) -> Option<String> {
        let name = self.full_name(cx);
        if name.is_empty() {
            return Some("Give it a name".into());
        }
        if let Some(problem) = spec_components::name_problem(&name) {
            return Some(problem.into());
        }
        if self.kind != ComponentKind::Scope && self.declared.iter().any(|(_, n)| *n == name) {
            return Some(format!("{name} is already declared in that file"));
        }
        None
    }

    fn folder_problem(&self, cx: &App) -> Option<String> {
        (self.kind == ComponentKind::Scope)
            .then(|| spec_components::folder_problem(&self.spec_dir, &self.folder.read(cx).value()))
            .flatten()
            .map(str::to_string)
    }

    fn problem(&self, cx: &App) -> Option<String> {
        self.name_problem(cx)
            .or_else(|| self.folder_problem(cx))
            .or_else(|| {
                (self.kind != ComponentKind::Scope && self.file_value(cx).is_none())
                    .then(|| "Choose the file it goes in".to_string())
            })
    }

    fn create_problem(&self, cx: &App) -> Option<String> {
        self.problem(cx).or_else(|| {
            self.description
                .read(cx)
                .value()
                .trim()
                .is_empty()
                .then(|| "Describe it".to_string())
        })
    }

    fn skill_problem(&self, cx: &App) -> Option<String> {
        self.problem(cx).or_else(|| {
            self.skill
                .read(cx)
                .selected_value()
                .is_none()
                .then(|| "Choose a skill to run".to_string())
        })
    }

    #[cfg(test)]
    pub fn fill(
        &mut self,
        name: &str,
        description: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.name.update(cx, |input, cx| {
            input.set_value(name.to_string(), window, cx)
        });
        self.description.update(cx, |editor, cx| {
            editor.set_value(description.to_string(), window, cx)
        });
        self.follow_name(window, cx);
        cx.notify();
    }

    /// Why Run Skill can't run yet, if it can't.
    #[cfg(test)]
    pub fn running_problem(&self, cx: &App) -> Option<String> {
        self.skill_problem(cx)
    }

    /// Why Create can't run yet, if it can't.
    #[cfg(test)]
    pub fn creating_problem(&self, cx: &App) -> Option<String> {
        self.create_problem(cx)
    }

    #[cfg(test)]
    pub fn choose_file(&mut self, file: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.file.update(cx, |select, cx| {
            select.set_selected_value(&file.to_string(), window, cx)
        });
        self.read_file(window, cx);
    }

    #[cfg(test)]
    pub fn set_skills(&mut self, skills: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let first = skills.first().cloned();
        self.skill.update(cx, |select, cx| {
            select.set_items(SearchableVec::new(skills), window, cx);
            if let Some(first) = first {
                select.set_selected_value(&first, window, cx);
            }
        });
    }

    /// Writes the component, then says so, or shows what went wrong.
    pub fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating || self.create_problem(cx).is_some() {
            return;
        }
        let request = self.request(cx);
        let spec_dir = self.spec_dir.clone();
        self.creating = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move { request.write(&spec_dir) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.creating = false;
                match result {
                    Ok(file) => cx.emit(ComponentCreated(file)),
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Hands the component to the chosen skill.
    pub fn run_skill(&mut self, cx: &mut Context<Self>) {
        if self.skill_problem(cx).is_some() {
            return;
        }
        let Some(skill) = self.skill.read(cx).selected_value().cloned() else {
            return;
        };
        cx.emit(RunComponentSkill(self.request(cx).skill_prompt(&skill)));
    }

    /// A labelled field, with a muted note and why its value can't be used
    /// beneath it.
    fn field(
        label: &'static str,
        control: impl IntoElement,
        note: Option<String>,
        problem: Option<String>,
        cx: &App,
    ) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .gap_1()
            .child(div().text_sm().font_medium().child(label))
            .child(control)
            .when_some(note, |field, note| {
                field.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(note),
                )
            })
            .when_some(problem, |field, problem| {
                field.child(div().text_xs().text_color(theme.danger).child(problem))
            })
    }
}

impl Render for SpecComponentForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let kind = self.kind;
        let name = self.full_name(cx);
        let typed = !self.name.read(cx).value().trim().is_empty();

        let heading = h_flex()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_1().font_semibold().child(kind.title()))
            .child(
                Button::new("spec-component-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close without creating anything")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseSpecComponent))),
            );

        let mut fields = v_flex().gap_5().child(Self::field(
            "Name",
            gpui_kit::TestSupportExt::test_support(
                div()
                    .id("spec-component-name")
                    .child(Input::new(&self.name)),
            ),
            (typed && name != self.name.read(cx).value().trim()).then(|| name.clone()),
            self.name_problem(cx).filter(|_| typed),
            cx,
        ));
        match kind {
            ComponentKind::Scope => {
                let folder = self.folder.read(cx).value().trim().to_string();
                fields = fields
                    .child(Self::field(
                        "Kind",
                        Select::new(&self.keyword).id("spec-component-kind"),
                        None,
                        None,
                        cx,
                    ))
                    .child(Self::field(
                        "Folder",
                        Input::new(&self.folder),
                        (!folder.is_empty()).then(|| format!("Writes {folder}/index.pi")),
                        self.folder_problem(cx).filter(|_| typed),
                        cx,
                    ));
            }
            ComponentKind::Concept | ComponentKind::Shape => {
                fields = fields.child(Self::field(
                    "File",
                    Select::new(&self.file)
                        .id("spec-component-file")
                        .placeholder("Choose a spec file")
                        .search_placeholder("Search the spec files"),
                    None,
                    None,
                    cx,
                ));
                if kind == ComponentKind::Concept {
                    fields = fields.child(Self::field(
                        "Shape",
                        Select::new(&self.shape).id("spec-component-shape"),
                        None,
                        None,
                        cx,
                    ));
                }
            }
        }
        let (height, _) = self.description_fit.heights(&self.description, window, cx);
        let description = gpui_kit::TestSupportExt::test_support(
            div()
                .id("spec-component-description")
                .relative()
                .child(Editor::new(&self.description).h(height))
                .child(GrowToFit::tracker(
                    &self.description,
                    cx.entity().downgrade(),
                    |this: &mut Self| &mut this.description_fit,
                )),
        );
        let theme = cx.theme();
        fields = fields
            .child(Self::field("Description", description, None, None, cx))
            .child(
                div()
                    .id("spec-component-error")
                    .text_sm()
                    .text_color(theme.danger)
                    .children(self.error.clone()),
            );

        let body = div()
            .id("spec-component-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(div().p_6().child(fields));

        let create_problem = self.create_problem(cx);
        let skill_problem = self.skill_problem(cx);
        let footer = h_flex()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme.border)
            .child(
                Button::new("spec-component-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseSpecComponent))),
            )
            .child(div().flex_1())
            .child(
                Select::new(&self.skill)
                    .id("spec-component-skill")
                    .placeholder("Skill")
                    .search_placeholder("Search the skills")
                    .w(px(240.)),
            )
            .child(
                Button::new("spec-component-run-skill")
                    .label("Run Skill")
                    .icon(IconName::Sparkles)
                    .disabled(skill_problem.is_some())
                    .tooltip(
                        skill_problem
                            .unwrap_or_else(|| "Close and hand it to the skill to create".into()),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.run_skill(cx))),
            )
            .child(
                Button::new("spec-component-create")
                    .primary()
                    .label("Create")
                    .loading(self.creating)
                    .disabled(create_problem.is_some() || self.creating)
                    .tooltip(
                        create_problem
                            .unwrap_or_else(|| "Write it into the spec and open it".into()),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
            );

        let form = v_flex()
            .id("spec-component")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .child(heading)
            .child(body)
            .child(footer);
        // Lets UI tests find the form; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(form)
    }
}
