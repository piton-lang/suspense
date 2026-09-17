//! New Instruction (see the NewInstructionScope): a Belay instruction for a
//! folder or file of the code, written by hand in the editor, in the folder
//! of the shape location that mirrors it. The inset panel first asks which
//! folder or file, and the anchor's name, then shows the file to be written in
//! the editor, not yet on disk, until it is saved.

use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::*;
use lsp_types::Position;

use crate::file_view::{CloseFile, FileView};
use crate::hidden_anchor;

actions!(suspense, [NewInstruction]);

/// The most folders and files offered.
const MAX_ENTRIES: usize = 5000;

/// Emitted to close the panel, with the instruction's file when it was
/// written.
pub struct CloseInstruction(pub Option<PathBuf>);

/// `path` without a leading `./` or trailing slash, with forward slashes.
fn normalize(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
}

/// Where a project's instructions go: its code location, and the location
/// mirroring it, the shape location, or the spec location when it has none,
/// with whether that is the spec location. All relative to the project.
#[derive(Clone, Debug, PartialEq)]
pub struct Locations {
    pub code_root: String,
    pub mirror_root: String,
    pub mirrors_spec: bool,
}

impl Locations {
    pub fn read(project_dir: &Path) -> Result<Self> {
        let code_root = normalize(&hidden_anchor::config_value(project_dir, "codeRoot")?);
        let (mirror_root, mirrors_spec) =
            match hidden_anchor::config_value(project_dir, "shapeRoot") {
                Ok(shape_root) => (normalize(&shape_root), false),
                Err(_) => (
                    normalize(&hidden_anchor::config_value(project_dir, "root")?),
                    true,
                ),
            };
        Ok(Self {
            code_root,
            mirror_root,
            mirrors_spec,
        })
    }

    /// The instruction file for `entry`, a folder (ending with a slash) or a
    /// file under the code location, named `name`: in the mirroring folder of
    /// the folder itself, or of the file's folder. Relative to the project.
    pub fn instruction_file(&self, entry: &str, name: &str) -> Option<String> {
        let is_folder = entry.ends_with('/');
        let entry = entry.trim_end_matches('/');
        let within = if entry == self.code_root {
            ""
        } else {
            entry.strip_prefix(&format!("{}/", self.code_root))?
        };
        let is_folder = is_folder || entry == self.code_root;
        let folder = if is_folder {
            within.to_string()
        } else {
            Path::new(within)
                .parent()
                .map(|parent| parent.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default()
        };
        let mut file = self.mirror_root.clone();
        if !folder.is_empty() {
            file.push('/');
            file.push_str(&folder);
        }
        Some(format!("{file}/{name}.pi"))
    }
}

/// The folders, each ending with a slash, and files under the code location,
/// the location itself first, then by path; relative to the project, leaving
/// out what Git ignores.
pub fn code_entries(project_dir: &Path, code_root: &str) -> Result<Vec<String>> {
    let dir = project_dir.join(code_root);
    if !dir.is_dir() {
        bail!("{} isn't a folder", dir.display());
    }
    let mut entries: Vec<String> = ignore::WalkBuilder::new(&dir)
        .hidden(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let is_dir = entry.file_type()?.is_dir();
            let relative = entry.path().strip_prefix(project_dir).ok()?;
            if !relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            {
                return None;
            }
            let path = relative.to_string_lossy().replace('\\', "/");
            Some(if is_dir { format!("{path}/") } else { path })
        })
        .take(MAX_ENTRIES)
        .collect();
    let root = format!("{code_root}/");
    entries.retain(|entry| *entry != root);
    entries.sort();
    entries.insert(0, root);
    Ok(entries)
}

/// A folder's or file's name, without its extension, in Pascal case:
/// `spec_tab.rs` as `SpecTab`, `file-tree/` as `FileTree`.
pub fn pascal_case(entry: &str) -> String {
    let is_folder = entry.ends_with('/');
    let entry = entry.trim_end_matches('/');
    let name = Path::new(entry)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = if is_folder {
        name.as_str()
    } else {
        name.split('.').next().unwrap_or_default()
    };
    stem.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            let first = chars.next().unwrap().to_ascii_uppercase();
            std::iter::once(first).chain(chars).collect::<String>()
        })
        .collect::<String>()
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .to_string()
}

/// The instruction's file as it starts, and where the cursor starts: on the
/// line of its prompt to write over.
pub fn template(name: &str, entry: &str) -> (String, Position) {
    let entry = entry.trim_end_matches('/').replace(':', "\\:");
    let text = format!(
        "use @piton/belay\n\nexport instruction {name}:\n    description: Instructions for {entry}\n    prompt:\n        Write the instructions here.\n"
    );
    (text, Position::new(5, 8))
}

enum Step {
    Choosing,
    Writing(Entity<FileView>),
}

pub struct NewInstructionForm {
    project_dir: PathBuf,
    locations: Option<Locations>,
    /// Why the locations could not be read.
    locations_error: Option<String>,
    /// The folder or file of the code it is for.
    code: Entity<SelectState<SearchableVec<String>>>,
    /// The anchor's name, following the folder or file until it is edited.
    name: Entity<InputState>,
    name_edited: bool,
    filling_name: bool,
    step: Step,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CloseInstruction> for NewInstructionForm {}

impl Focusable for NewInstructionForm {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl NewInstructionForm {
    pub fn new(project_dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (locations, locations_error, entries) = match Locations::read(&project_dir) {
            Ok(locations) => match code_entries(&project_dir, &locations.code_root) {
                Ok(entries) => (Some(locations), None, entries),
                Err(err) => (Some(locations), Some(format!("{err:#}")), Vec::new()),
            },
            Err(err) => (None, Some(format!("{err:#}")), Vec::new()),
        };
        let code = cx.new(|cx| {
            SelectState::new(SearchableVec::new(entries), None, window, cx).searchable(true)
        });
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("SpecTab"));
        let subscriptions = vec![
            cx.subscribe_in(
                &code,
                window,
                |this, _, _: &SelectEvent<SearchableVec<String>>, window, cx| {
                    this.follow_code(window, cx);
                    cx.notify();
                },
            ),
            cx.subscribe(&name, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    if !this.filling_name {
                        this.name_edited = true;
                    }
                    cx.notify();
                }
            }),
        ];
        code.update(cx, |code, cx| code.focus(window, cx));
        Self {
            project_dir,
            locations,
            locations_error,
            code,
            name,
            name_edited: false,
            filling_name: false,
            step: Step::Choosing,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn entry(&self, cx: &App) -> Option<String> {
        self.code.read(cx).selected_value().cloned()
    }

    /// Fills in the name from the chosen folder or file, until it is edited.
    fn follow_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.name_edited {
            return;
        }
        let Some(entry) = self.entry(cx) else {
            return;
        };
        let name = pascal_case(&entry);
        self.filling_name = true;
        self.name
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.filling_name = false;
    }

    /// The instruction's file, relative to the project, once a folder or file
    /// is chosen.
    pub fn file(&self, cx: &App) -> Option<String> {
        let entry = self.entry(cx)?;
        let name = self.name.read(cx).value().trim().to_string();
        self.locations.as_ref()?.instruction_file(&entry, &name)
    }

    /// Why Continue can't go on yet, if it can't.
    fn problem(&self, cx: &App) -> Option<String> {
        if let Some(error) = &self.locations_error {
            return Some(error.clone());
        }
        let Some(entry) = self.entry(cx) else {
            return Some("Choose a folder or file of the code".into());
        };
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return Some("Give the instruction a name".into());
        }
        if let Some(problem) = crate::spec_components::name_problem(&name) {
            return Some(problem.into());
        }
        let file = self
            .locations
            .as_ref()
            .and_then(|locations| locations.instruction_file(&entry, &name))?;
        self.project_dir
            .join(&file)
            .exists()
            .then(|| format!("{file} already exists"))
    }

    /// Whether the instruction is being written and has changes not saved.
    pub fn is_dirty(&self, cx: &App) -> bool {
        match &self.step {
            Step::Writing(file) => file.read(cx).is_dirty(),
            Step::Choosing => false,
        }
    }

    /// The editor's title, for asking to discard its changes.
    pub fn title(&self, cx: &App) -> SharedString {
        match &self.step {
            Step::Writing(file) => file.read(cx).title(),
            Step::Choosing => "The instruction".into(),
        }
    }

    #[cfg(test)]
    pub fn choose(&mut self, entry: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.code.update(cx, |code, cx| {
            code.set_selected_value(&entry.to_string(), window, cx)
        });
        self.follow_code(window, cx);
        cx.notify();
    }

    #[cfg(test)]
    pub fn file_view(&self) -> Option<Entity<FileView>> {
        match &self.step {
            Step::Writing(file) => Some(file.clone()),
            Step::Choosing => None,
        }
    }

    /// Goes on to writing the instruction, in the editor, not yet on disk.
    pub fn go_on(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.problem(cx).is_some() {
            return;
        }
        let (Some(entry), Some(file)) = (self.entry(cx), self.file(cx)) else {
            return;
        };
        let name = self.name.read(cx).value().trim().to_string();
        let (text, cursor) = template(&name, &entry);
        let path = self.project_dir.join(file);
        let view = cx.new(|cx| FileView::unwritten(path, text, cursor, window, cx));
        self._subscriptions
            .push(cx.subscribe(&view, |_, view, _: &CloseFile, cx| {
                let path = view.read(cx).path().to_path_buf();
                cx.emit(CloseInstruction(path.exists().then_some(path)));
            }));
        view.update(cx, |view, cx| view.focus_editor(window, cx));
        self.step = Step::Writing(view);
        cx.notify();
    }

    fn render_choosing(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let problem = self.problem(cx);
        let chosen = self.entry(cx).is_some();
        let typed = !self.name.read(cx).value().trim().is_empty();
        let note = match (&self.file(cx), &self.locations) {
            (Some(file), Some(locations)) if typed => {
                let mut note = format!("Writes {file}");
                if locations.mirrors_spec {
                    note.push_str(", in the spec location, as the project has no shapeRoot");
                }
                Some(note)
            }
            _ => None,
        };

        let heading = h_flex()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_1().font_semibold().child("New Instruction"))
            .child(
                Button::new("new-instruction-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close without writing an instruction")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseInstruction(None)))),
            );
        let label = |text: &'static str| div().text_sm().font_medium().child(text);
        let body = v_flex()
            .id("new-instruction-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_6()
            .gap_5()
            .child(
                v_flex().gap_1().child(label("Code")).child(
                    Select::new(&self.code)
                        .id("new-instruction-code")
                        .placeholder("Choose a folder or file")
                        .search_placeholder("Search the code"),
                ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(label("Name"))
                    .child(Input::new(&self.name)),
            )
            .children(note.map(|note| {
                // Lets UI tests find the note; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(
                    div()
                        .id("new-instruction-location")
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(note),
                )
            }))
            .children(
                problem
                    .clone()
                    .filter(|_| chosen || self.locations_error.is_some())
                    .map(|problem| div().text_xs().text_color(theme.danger).child(problem)),
            );
        let footer = h_flex()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme.border)
            .child(div().flex_1())
            .child(
                Button::new("new-instruction-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseInstruction(None)))),
            )
            .child(
                Button::new("new-instruction-continue")
                    .primary()
                    .label("Continue")
                    .disabled(problem.is_some())
                    .tooltip(problem.unwrap_or_else(|| "Write the instruction".into()))
                    .on_click(cx.listener(|this, _, window, cx| this.go_on(window, cx))),
            );
        v_flex()
            .size_full()
            .child(heading)
            .child(body)
            .child(footer)
    }
}

impl Render for NewInstructionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.step {
            Step::Writing(file) => file.clone().into_any_element(),
            Step::Choosing => self.render_choosing(cx).into_any_element(),
        };
        // Tracks focus around the editor too, so the panel keeps it inside.
        let form = div()
            .id("new-instruction")
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
    use std::path::Path;

    use super::{Locations, code_entries, pascal_case, template};

    fn locations(shape: bool) -> Locations {
        Locations {
            code_root: "src".into(),
            mirror_root: if shape { "spec/shape" } else { "spec" }.into(),
            mirrors_spec: !shape,
        }
    }

    #[test]
    fn names_come_from_the_folder_or_file() {
        assert_eq!(pascal_case("src/ribbon/spec_tab.rs"), "SpecTab");
        assert_eq!(pascal_case("src/file-tree/"), "FileTree");
        assert_eq!(pascal_case("src/main.rs"), "Main");
        assert_eq!(pascal_case("src/"), "Src");
    }

    #[test]
    fn instructions_mirror_the_code() {
        let shape = locations(true);
        assert_eq!(
            shape
                .instruction_file("src/ribbon/spec_tab.rs", "SpecTab")
                .as_deref(),
            Some("spec/shape/ribbon/SpecTab.pi")
        );
        assert_eq!(
            shape.instruction_file("src/ribbon/", "Ribbon").as_deref(),
            Some("spec/shape/ribbon/Ribbon.pi")
        );
        assert_eq!(
            shape.instruction_file("src/main.rs", "Main").as_deref(),
            Some("spec/shape/Main.pi")
        );
        assert_eq!(
            shape.instruction_file("src/", "Src").as_deref(),
            Some("spec/shape/Src.pi")
        );
        assert_eq!(
            locations(false)
                .instruction_file("src/a/b.rs", "B")
                .as_deref(),
            Some("spec/a/B.pi")
        );
        assert_eq!(shape.instruction_file("other/x.rs", "X"), None);
    }

    /// The instruction as it starts is Belay Piton that checks.
    #[test]
    fn the_template_checks() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/new-instruction-test");
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(project.join("spec/shape/ribbon")).unwrap();
        std::fs::create_dir_all(project.join("src/ribbon")).unwrap();
        std::fs::write(
            project.join("piton.config.pi"),
            "use @piton/config\nuse @piton/belay\n\nexport piton-config Project:\n    root: ./spec\n    entry: ./spec/index.pi\n\n    frameworks:\n        - {Belay}\n\nbelay-config Belay:\n    codeRoot: ./src\n    shapeRoot: ./spec/shape\n",
        )
        .unwrap();
        std::fs::write(project.join("spec/index.pi"), "").unwrap();
        let (text, cursor) = template("SpecTab", "src/ribbon/spec_tab.rs");
        assert_eq!(
            text.lines().nth(cursor.line as usize),
            Some("        Write the instructions here.")
        );
        let file = project.join("spec/shape/ribbon/SpecTab.pi");
        std::fs::write(&file, &text).unwrap();
        let out = std::process::Command::new("piton")
            .arg("check")
            .arg(&file)
            .current_dir(&project)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{text}\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let entries = code_entries(&project, "src").unwrap();
        assert_eq!(entries, ["src/", "src/ribbon/"]);
        std::fs::remove_dir_all(&project).ok();
    }
}
