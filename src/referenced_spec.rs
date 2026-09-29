//! The sidebar at the right of the task view while a task runs, listing the
//! spec files it references: those its prompt imports from, then those the
//! harness or a subagent it starts reads, edits, or writes, whether with a
//! file tool, a shell command, or a search naming them, in the spec location
//! or under `.claude/reference`, each once; and beneath them, the task's
//! understanding: the constraints the harness has taken from the spec.
//! Its panels' headers are on the darkest surface and its body on the
//! ribbon's command area colour, with no line anywhere in it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::file_link::OpenFile;
use crate::harness::HarnessEvent;
use crate::shell_paths::{self, Scope};
use crate::understanding::Understanding;

/// How wide the sidebar starts, and how narrow it can be dragged.
pub const WIDTH: Pixels = px(260.);
pub const MIN_WIDTH: Pixels = px(160.);

/// Where the reference files built from the spec are, in the project.
const REFERENCE_DIR: &str = ".claude/reference";

/// A spec file the task references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Referenced {
    pub path: PathBuf,
    /// Whether it was edited or written, rather than only read or imported.
    pub edited: bool,
    /// Whether a subagent the harness started referenced it.
    pub by_subagent: bool,
}

/// Every file a run's tool calls reference, wherever it is, in the order
/// first referenced, as the run's events arrive.
#[derive(Default)]
pub struct References {
    /// Where relative paths are read from.
    project_dir: PathBuf,
    files: Vec<Referenced>,
    /// The calls, by id, that take files in by folders or patterns, whose
    /// output names the files they count.
    scopes: HashMap<String, Vec<Scope>>,
}

impl References {
    pub fn new(project_dir: Option<PathBuf>) -> Self {
        Self {
            project_dir: project_dir.unwrap_or_default(),
            ..Self::default()
        }
    }

    /// Notes the files a tool call's input or output references.
    pub fn apply(&mut self, event: &HarnessEvent) {
        match event {
            HarnessEvent::ToolCalled {
                id,
                name,
                input,
                subagent,
            } => self.called(id, name, input, *subagent),
            HarnessEvent::ToolOutput {
                id,
                output,
                subagent,
            } => {
                let Some(scopes) = self.scopes.remove(id) else {
                    return;
                };
                for line in output.lines() {
                    if let Some(path) = scopes.iter().find_map(|scope| scope.named_in(line)) {
                        self.note(path, false, *subagent);
                    }
                }
            }
            _ => {}
        }
    }

    fn called(&mut self, id: &str, name: &str, input: &Value, subagent: bool) {
        let text = |key: &str| input.get(key).and_then(Value::as_str);
        let path = |text: &str| shell_paths::normalize(&self.project_dir.join(text));
        match name {
            "Read" | "NotebookRead" | "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
                let Some(file) = text("file_path").or_else(|| text("notebook_path")) else {
                    return;
                };
                let edited = !matches!(name, "Read" | "NotebookRead");
                self.note(path(file), edited, subagent);
            }
            "Grep" | "Glob" => {
                let dir = text("path").map_or_else(|| self.project_dir.clone(), path);
                let scope = Scope {
                    dir,
                    base: self.project_dir.clone(),
                };
                self.scopes.insert(id.to_string(), vec![scope]);
            }
            "Bash" => {
                let Some(command) = text("command") else {
                    return;
                };
                let found = shell_paths::analyze(command, &self.project_dir);
                for (file, edited) in found.files {
                    self.note(file, edited, subagent);
                }
                if !found.scopes.is_empty() {
                    self.scopes.insert(id.to_string(), found.scopes);
                }
            }
            _ => {}
        }
    }

    fn note(&mut self, path: PathBuf, edited: bool, by_subagent: bool) {
        match self.files.iter_mut().find(|file| file.path == path) {
            Some(file) => {
                file.edited |= edited;
                file.by_subagent |= by_subagent;
            }
            None => self.files.push(Referenced {
                path,
                edited,
                by_subagent,
            }),
        }
    }
}

/// The spec files a task references: `imported`, the files its prompt imports
/// from, then those its run references in `spec_dir` or the reference files,
/// each once, in the order first referenced.
pub fn collect(
    imported: &[PathBuf],
    references: &References,
    project_dir: &Path,
    spec_dir: Option<&Path>,
) -> Vec<Referenced> {
    let reference_dir = project_dir.join(REFERENCE_DIR);
    let mut files: Vec<Referenced> = imported
        .iter()
        .map(|path| Referenced {
            path: shell_paths::normalize(path),
            edited: false,
            by_subagent: false,
        })
        .collect();
    for referenced in &references.files {
        let path = &referenced.path;
        let is_spec =
            spec_dir.is_some_and(|dir| path.starts_with(dir)) || path.starts_with(&reference_dir);
        if !is_spec {
            continue;
        }
        match files.iter_mut().find(|file| file.path == *path) {
            Some(file) => {
                file.edited |= referenced.edited;
                file.by_subagent |= referenced.by_subagent;
            }
            None => files.push(referenced.clone()),
        }
    }
    files
}

/// What a row shows of `path`: its file name, after the folder it is in when
/// it is an index file, as `chat-input/index.pi`.
fn shown_name(path: &Path) -> String {
    let Some(name) = path.file_name() else {
        return path.display().to_string();
    };
    let name = name.to_string_lossy();
    let is_index = path.file_stem().is_some_and(|stem| stem == "index");
    match path.parent().and_then(Path::file_name).filter(|_| is_index) {
        Some(folder) => format!("{}/{name}", folder.to_string_lossy()),
        None => name.into_owned(),
    }
}

/// How tall a referenced file's row is.
pub const ROW_HEIGHT: Pixels = px(24.);

/// How tall an understanding row's box is for a single line, and how far
/// apart the boxes are.
pub const BOX_HEIGHT: Pixels = px(32.);
pub const BOX_GAP: Pixels = px(4.);

/// How tall a line of an understanding row's text is, so that a single line
/// makes its box 32 pixels tall.
const BOX_LINE_HEIGHT: Pixels = px(20.);

/// How rounded an understanding row's box is.
const BOX_RADIUS: Pixels = px(4.);

/// How much of the sidebar's height the referenced files can take before
/// they scroll.
const FILES_MAX_SHARE: f32 = 0.5;

/// Where the sidebar's panels scroll.
pub struct Layout<'a> {
    pub files_scroll: &'a ScrollHandle,
    pub understanding_scroll: &'a ScrollHandle,
}

/// How tall the referenced files' panel is to show `rows` rows, before it
/// is held to half the sidebar: its header, and its rows inside its padding.
/// While there are none, its placeholder takes a row.
pub fn files_height(rows: usize) -> Pixels {
    crate::sidebar::HEADER_HEIGHT + crate::sidebar::PADDING * 2. + ROW_HEIGHT * rows.max(1) as f32
}

/// A panel's body: its `list`, which scrolls on its own with `scroll`, with a scrollbar at
/// its right only while it is taller than the panel, so that nothing draws a
/// line beside rows that all fit. Whether it overflows is known once it is
/// laid out, so the frame is drawn again when that changes.
fn body(id: &'static str, scroll: &ScrollHandle, list: impl IntoElement, cx: &App) -> AnyElement {
    let overflows = scroll.max_offset().y > px(0.);
    let watch = {
        let scroll = scroll.clone();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                if (scroll.max_offset().y > px(0.)) != overflows {
                    window.refresh();
                }
            },
        )
        .absolute()
        .size_0()
    };
    let content = if overflows {
        crate::scrollbar::with_scrollbar(id, scroll, list, true, None, cx)
    } else {
        list.into_any_element()
    };
    div()
        .relative()
        .flex_1()
        .min_h_0()
        .w_full()
        .child(content)
        .child(watch)
        .into_any_element()
}

/// A muted line standing in for rows while there are none.
fn placeholder(text: &'static str, cx: &App) -> Div {
    h_flex()
        .h(ROW_HEIGHT)
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// The sidebar's contents, with no line anywhere: the referenced files at
/// the top, only as tall as their rows need, up to half its height, each row
/// opening its file; and the task's understanding filling the rest, a box
/// per constraint, opening the file it links to.
pub fn render(
    files: &[Referenced],
    understanding: &Understanding,
    layout: Layout,
    open: OpenFile,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let palette = crate::theme::palette(cx);
    let rows = files.iter().enumerate().map(|(ix, file)| {
        // Only the file's name, and for an index file the folder it is in;
        // its full path is in its tooltip.
        let shown = shown_name(&file.path);
        let full: SharedString = file.path.display().to_string().into();
        let by_subagent = file.by_subagent;
        let (path, open) = (file.path.clone(), open.clone());
        // Lets UI tests find each row; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(h_flex().id(("referenced-file", ix)))
            .flex_none()
            .h(ROW_HEIGHT)
            .gap_2()
            .text_sm()
            .cursor_pointer()
            .hover(|row| row.bg(theme.list_hover))
            .child(div().flex_1().min_w_0().truncate().child(shown))
            .when(file.edited, |row| {
                row.child(
                    Icon::new(IconName::Pencil)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
            })
            .tooltip(move |window, cx| {
                let full = full.clone();
                Tooltip::element(move |_, cx| {
                    v_flex().child(full.clone()).when(by_subagent, |tooltip| {
                        tooltip.child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child("Read by a subagent"),
                        )
                    })
                })
                .build(window, cx)
            })
            .on_click(move |_, window, cx| open(path.clone(), window, cx))
    });
    // Lets UI tests find the list; inert in normal builds.
    let files_list = gpui_kit::TestSupportExt::test_support(v_flex().id("referenced-files-list"))
        .size_full()
        .overflow_y_scroll()
        .track_scroll(layout.files_scroll)
        .p(crate::sidebar::PADDING)
        .when(files.is_empty(), |list| {
            list.child(placeholder("No spec files referenced yet", cx))
        })
        .children(rows);

    let boxes = crate::theme::color(palette.ribbon_tabs);
    let constraints = understanding.rows.iter().enumerate().map(|(ix, row)| {
        let highlight = Understanding::highlight(row);
        let missing = row.linked && row.target.is_none();
        // Lets UI tests find each row; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(v_flex().id(("understanding-row", ix)))
            .flex_none()
            .w_full()
            .min_h(BOX_HEIGHT)
            .justify_center()
            .py((BOX_HEIGHT - BOX_LINE_HEIGHT) / 2.)
            .px(crate::sidebar::PADDING)
            .rounded(BOX_RADIUS)
            .text_sm()
            .line_height(BOX_LINE_HEIGHT)
            // A row just added or changed is lit, fading back to its box.
            .bg(boxes.blend(theme.accent.opacity(highlight.clamp(0., 1.))))
            .when(missing, |this| this.text_color(theme.muted_foreground))
            .child(row.text.clone())
            .when_some(row.target.clone(), |this, target| {
                let open = open.clone();
                this.cursor_pointer()
                    .hover(|this| this.bg(theme.list_hover))
                    .on_click(move |_, window, cx| open(target.clone(), window, cx))
            })
    });
    // Lets UI tests find the list; inert in normal builds.
    let understanding_list =
        gpui_kit::TestSupportExt::test_support(v_flex().id("understanding-list"))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(layout.understanding_scroll)
            .p(crate::sidebar::PADDING)
            .gap(BOX_GAP)
            .when(!understanding.exists, |list| {
                list.child(placeholder("Nothing understood yet", cx))
            })
            .children(constraints);

    // Lets UI tests find each part; inert in normal builds.
    let find =
        |id: &'static str, element: Div| gpui_kit::TestSupportExt::test_support(element.id(id));
    find("referenced-files", v_flex())
        .size_full()
        .overflow_hidden()
        .bg(crate::theme::color(palette.ribbon))
        .child(
            find("referenced-files-panel", v_flex())
                .flex_none()
                .w_full()
                .h(files_height(files.len()))
                .max_h(relative(FILES_MAX_SHARE))
                .child(find(
                    "referenced-files-header",
                    crate::sidebar::header("Referenced Spec", cx),
                ))
                .child(body(
                    "referenced-files",
                    layout.files_scroll,
                    files_list,
                    cx,
                )),
        )
        .child(
            find("understanding-panel", v_flex())
                .flex_1()
                .min_h_0()
                .w_full()
                .child(find(
                    "understanding-header",
                    crate::sidebar::header("Understanding", cx),
                ))
                .child(body(
                    "understanding",
                    layout.understanding_scroll,
                    understanding_list,
                    cx,
                )),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::{Referenced, References, collect, shown_name};
    use crate::harness::HarnessEvent;

    /// A project with a few spec, reference, and source files.
    fn project() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("suspense-refs-collect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for file in [
            "spec/a.pi",
            "spec/b/index.pi",
            "spec/c.pi",
            "spec/d.pi",
            "spec/unnamed.pi",
            ".claude/reference/A.md",
            "src/main.rs",
        ] {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x\n").unwrap();
        }
        dir
    }

    fn call(id: &str, name: &str, input: serde_json::Value, subagent: bool) -> HarnessEvent {
        HarnessEvent::ToolCalled {
            id: id.into(),
            name: name.into(),
            input,
            subagent,
        }
    }

    fn output(id: &str, output: &str, subagent: bool) -> HarnessEvent {
        HarnessEvent::ToolOutput {
            id: id.into(),
            output: output.into(),
            subagent,
        }
    }

    /// Imports come first, then the spec files referenced however they are:
    /// by file tools, shell commands, searches whose results name them, and
    /// subagents; each once, an edit marking a file edited and a subagent's
    /// reference marking it so. Files elsewhere, and files only taken in by a
    /// folder or pattern, are left out.
    #[test]
    fn lists_imports_then_spec_files_referenced_once() {
        let dir = project();
        let mut references = References::new(Some(dir.clone()));
        let long = format!("spec/{}/index.pi", "deep".repeat(40));
        let path = |file: &str| dir.join(file).display().to_string();
        for event in [
            call(
                "1",
                "Read",
                json!({ "file_path": path("spec/a.pi") }),
                false,
            ),
            call(
                "2",
                "Read",
                json!({ "file_path": path("src/main.rs") }),
                false,
            ),
            call(
                "3",
                "Bash",
                json!({ "command": "cat .claude/reference/A.md" }),
                false,
            ),
            call(
                "4",
                "Edit",
                json!({ "file_path": path("spec/a.pi") }),
                false,
            ),
            call(
                "5",
                "Read",
                json!({ "file_path": path("spec/b/index.pi") }),
                false,
            ),
            call("6", "Write", json!({ "file_path": path(&long) }), false),
            call(
                "7",
                "Grep",
                json!({ "pattern": "x", "path": path("spec") }),
                false,
            ),
            output("7", "Found 2 files\nspec/c.pi\nsrc/main.rs", false),
            call(
                "8",
                "Bash",
                json!({ "command": "echo x > spec/c.pi" }),
                false,
            ),
            call("9", "Glob", json!({ "pattern": "spec/*.pi" }), true),
            output("9", &path("spec/d.pi"), true),
            call("10", "Bash", json!({ "command": "grep -rl x spec" }), false),
        ] {
            references.apply(&event);
        }
        let imported = [dir.join("spec/b/index.pi")];
        let files = collect(&imported, &references, &dir, Some(&dir.join("spec")));
        let expected = [
            ("spec/b/index.pi", false, false),
            ("spec/a.pi", true, false),
            (".claude/reference/A.md", false, false),
            (long.as_str(), true, false),
            ("spec/c.pi", true, false),
            ("spec/d.pi", false, true),
        ]
        .map(|(path, edited, by_subagent)| Referenced {
            path: dir.join(path),
            edited,
            by_subagent,
        });
        assert_eq!(files, expected);
        assert!(
            !files
                .iter()
                .any(|file| file.path == Path::new(&path("spec/unnamed.pi")))
        );
    }

    /// A row shows the file's name, and an index file's folder before it.
    #[test]
    fn shows_names_and_index_folders() {
        assert_eq!(shown_name(Path::new("/p/spec/a.pi")), "a.pi");
        assert_eq!(
            shown_name(Path::new("/p/spec/prompt-mode/chat-input/index.pi")),
            "chat-input/index.pi"
        );
        assert_eq!(
            shown_name(Path::new("/p/.claude/reference/index.md")),
            "reference/index.md"
        );
        assert_eq!(shown_name(Path::new("/p/spec/indexes.pi")), "indexes.pi");
    }
}
