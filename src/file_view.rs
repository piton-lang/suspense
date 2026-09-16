//! A file opened from the project tree, in an editor: syntax highlighted,
//! saved with Ctrl/Cmd+S, and for Piton files backed by the project's
//! `piton lsp` for completions, hover, definitions, diagnostics and quick
//! fixes. A header above it shows its path in the project, whether it has
//! unsaved changes, and save and close buttons.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::input::{
    CodeActionProvider, CompletionProvider, DefinitionProvider, Editor, EditorState, Enter,
    HoverProvider, InputEvent, Rope, ShowDocumentHandler,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lsp_types::{
    CodeAction, CodeActionOrCommand, CompletionContext, CompletionResponse, CompletionTextEdit,
    DocumentChanges, GotoDefinitionResponse, Hover, Location, LocationLink, OneOf, Position,
    TextEdit, WorkspaceEdit,
};
use serde_json::{Value, json};

use crate::piton_lsp::{self, LspClient};
use crate::piton_syntax;
use crate::project_directory::ProjectDirectory;
use crate::project_lsp::ProjectLsp;

actions!(file_view, [SaveFile]);

const CONTEXT: &str = "FileView";

/// How often what the server published is collected.
const DIAGNOSTICS_INTERVAL: Duration = Duration::from_millis(100);

#[cfg(target_os = "macos")]
const SAVE_SHORTCUT: &str = "⌘S";
#[cfg(not(target_os = "macos"))]
const SAVE_SHORTCUT: &str = "Ctrl+S";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-s", SaveFile, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-s", SaveFile, Some(CONTEXT)),
    ]);
}

/// Emitted when the file is to be closed.
pub struct CloseFile;

/// Emitted to open the file holding a definition, at the definition.
pub struct OpenDefinition {
    pub path: PathBuf,
    pub position: Position,
}

/// The editor is never narrower than this many columns of text.
pub const MIN_COLUMNS: usize = 80;

/// Columns beside the line numbers' digits: their margins, the fold icons,
/// and the editor's padding and scrollbar.
const GUTTER_EXTRA_COLUMNS: usize = 6;

pub struct FileView {
    path: PathBuf,
    /// The file's path within the project, or in full outside of one.
    title: SharedString,
    editor: Entity<EditorState>,
    /// Why the file could not be shown, once reading it failed.
    error: Option<SharedString>,
    /// Why the file could not be saved, until it next saves.
    save_error: Option<SharedString>,
    /// The text as last read from or written to disk; `None` until read.
    saved: Option<String>,
    dirty: bool,
    /// The file as open in `piton lsp`, while it is a Piton file and the
    /// server is running.
    document: Option<Arc<Document>>,
    /// What the server last published for the file.
    diagnostics: Vec<lsp_types::Diagnostic>,
    _load: Task<()>,
    _save: Task<()>,
    _watch_diagnostics: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CloseFile> for FileView {}
impl EventEmitter<OpenDefinition> for FileView {}

impl FileView {
    /// Opens the file at `path`, with the cursor at `position` if given.
    pub fn new(
        path: PathBuf,
        position: Option<Position>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = match ProjectDirectory::get(cx) {
            Some(root) => path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .display()
                .to_string(),
            None => path.display().to_string(),
        };
        // Highlighting is chosen by extension; one it does not know is plain text.
        let language = match path.extension().and_then(|ext| ext.to_str()) {
            Some("pi") => piton_syntax::LANGUAGE_NAME.to_string(),
            Some(ext) => ext.to_string(),
            None => String::new(),
        };
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .soft_wrap(false)
                .placeholder("Loading…")
        });

        let subscriptions = vec![
            cx.subscribe_in(
                &editor,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.on_edit(window, cx);
                    }
                },
            ),
            cx.observe_global_in::<ProjectLsp>(window, |this, _, cx| this.attach_lsp(cx)),
        ];

        let read = cx.background_spawn({
            let path = path.clone();
            async move { std::fs::read_to_string(path) }
        });
        let load = cx.spawn_in(window, async move |this, cx| {
            let text = read.await;
            this.update_in(cx, |this, window, cx| {
                match text {
                    Ok(text) => {
                        this.editor.update(cx, |editor, cx| {
                            editor.set_value(text, window, cx);
                            if let Some(position) = position {
                                editor.set_cursor_position(position, window, cx);
                            }
                        });
                        // As the editor holds it, line endings normalized.
                        this.saved = Some(this.editor.read(cx).value().to_string());
                        this.attach_lsp(cx);
                    }
                    Err(err) => {
                        this.error = Some(format!("Could not show this file: {err}").into())
                    }
                }
                cx.notify();
            })
            .ok();
        });

        Self {
            path,
            title: title.into(),
            editor,
            error: None,
            save_error: None,
            saved: None,
            dirty: false,
            document: None,
            diagnostics: Vec::new(),
            _load: load,
            _save: Task::ready(()),
            _watch_diagnostics: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// Whether the file has changes that have not been saved.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Asks whether to discard the unsaved changes to the file titled
    /// `title`, and runs `discard` if so.
    pub fn confirm_discard(
        title: SharedString,
        window: &mut Window,
        cx: &mut App,
        discard: impl Fn(&mut Window, &mut App) + 'static,
    ) {
        let discard = Rc::new(discard);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let discard = discard.clone();
            alert
                .title("Discard unsaved changes?")
                .description(SharedString::from(format!(
                    "{title} has changes that have not been saved."
                )))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Discard")
                        .ok_variant(ButtonVariant::Danger)
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    discard(window, cx);
                    true
                })
        });
    }

    #[cfg(test)]
    pub fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::Focusable as _;
        self.editor.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn on_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(saved) = &self.saved else {
            return;
        };
        let text = self.editor.read(cx).value();
        self.dirty = text.as_ref() != saved;

        if let Some(document) = &self.document {
            document.sync(&text);
            let imports = document.take_accepted_imports(&text);
            if !imports.is_empty() {
                self.editor.update(cx, |editor, cx| {
                    editor.apply_lsp_edits(&imports, window, cx)
                });
            }
        }
        // The editor drops its diagnostics on every edit.
        self.show_diagnostics(cx);
        cx.notify();
    }

    /// Writes the file to disk. A Piton file is first formatted with
    /// `piton format`, and the editor shows the formatted text; one that
    /// cannot be formatted is saved as it is.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saved.is_none() {
            return;
        }
        let typed = self.editor.read(cx).value().to_string();
        let is_piton = self.path.extension().is_some_and(|ext| ext == "pi");
        let project_dir = ProjectDirectory::get(cx);
        let write = cx.background_spawn({
            let path = self.path.clone();
            let typed = typed.clone();
            async move {
                let text = if is_piton {
                    let dir = project_dir
                        .or_else(|| path.parent().map(Path::to_path_buf))
                        .unwrap_or_default();
                    format_piton(&typed, &dir).unwrap_or(typed)
                } else {
                    typed
                };
                std::fs::write(path, &text).map(|()| text)
            }
        });
        self._save = cx.spawn_in(window, async move |this, cx| {
            let written = write.await;
            this.update_in(cx, |this, window, cx| {
                match written {
                    Ok(text) => {
                        this.save_error = None;
                        // Shows the formatted text, unless the file was edited
                        // again while it saved.
                        if text != typed && this.editor.read(cx).value().as_ref() == typed {
                            this.editor.update(cx, |editor, cx| {
                                editor.set_value(text.clone(), window, cx)
                            });
                            if let Some(document) = &this.document {
                                document.sync(&text);
                            }
                        }
                        this.dirty = this.editor.read(cx).value().as_ref() != text;
                        this.saved = Some(text);
                        if let Some(document) = &this.document {
                            document.did_save();
                        }
                    }
                    Err(err) => {
                        this.save_error = Some(format!("Could not save this file: {err}").into())
                    }
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dirty {
            cx.emit(CloseFile);
            return;
        }
        let this = cx.entity().downgrade();
        Self::confirm_discard(self.title.clone(), window, cx, move |_, cx| {
            this.update(cx, |_, cx| cx.emit(CloseFile)).ok();
        });
    }

    /// Opens the file in the project's `piton lsp` and points the editor's
    /// language support at it, or takes that away when the file is not Piton
    /// or the server is not running.
    fn attach_lsp(&mut self, cx: &mut Context<Self>) {
        let is_piton = self.path.extension().is_some_and(|ext| ext == "pi");
        let client = ProjectLsp::get(cx).filter(|_| is_piton && self.saved.is_some());
        let attached = self
            .document
            .as_ref()
            .map(|document| Arc::as_ptr(&document.client));
        if client.as_ref().map(Arc::as_ptr) == attached {
            return;
        }

        let text = self.editor.read(cx).value();
        self.document = client.map(|client| Arc::new(Document::open(client, &self.path, &text)));
        self.diagnostics = Vec::new();
        self._watch_diagnostics = Task::ready(());

        let file = self
            .document
            .clone()
            .map(|document| Rc::new(PitonFile { document }));
        let this = cx.entity().downgrade();
        self.editor.update(cx, |editor, _| {
            let lsp = editor.lsp_mut();
            lsp.completion_provider = file.clone().map(|file| file as Rc<dyn CompletionProvider>);
            lsp.hover_provider = file.clone().map(|file| file as Rc<dyn HoverProvider>);
            lsp.definition_provider = file.clone().map(|file| file as Rc<dyn DefinitionProvider>);
            lsp.code_action_providers = file
                .clone()
                .map(|file| file as Rc<dyn CodeActionProvider>)
                .into_iter()
                .collect();
            lsp.show_document = file.map(|file| show_document(file.document.path.clone(), this));
        });

        if let Some(document) = &self.document {
            self.diagnostics = document.client.diagnostics(&document.path);
            let published = document.client.watch_diagnostics();
            let path = document.path.clone();
            // Collected on a timer rather than awaited: the client reports
            // from its own thread, which must not wake app tasks (tests forbid
            // it).
            self._watch_diagnostics = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(DIAGNOSTICS_INTERVAL).await;
                    let changed: Vec<PathBuf> = published.try_iter().collect();
                    if !changed.contains(&path) {
                        continue;
                    }
                    let updated = this.update(cx, |this, cx| {
                        if let Some(document) = &this.document {
                            this.diagnostics = document.client.diagnostics(&path);
                        }
                        this.show_diagnostics(cx);
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            });
        }
        self.show_diagnostics(cx);
    }

    fn show_diagnostics(&mut self, cx: &mut Context<Self>) {
        let diagnostics = &self.diagnostics;
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().clone();
            if let Some(set) = editor.diagnostics_mut() {
                set.reset(&text);
                set.extend(diagnostics.iter().cloned());
            }
            cx.notify();
        });
    }

    /// gpui-kit's completion and code action menus accept on Enter but let
    /// the keystroke carry on, which also types a newline: hand the Enter to
    /// the menu here and stop it once the menu takes it.
    fn route_enter_to_menus(
        &mut self,
        action: &Enter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = self.editor.read(cx);
        if !editor.completion_menu_state().open && !editor.code_action_menu_state().open {
            return;
        }
        let action = action.clone();
        let accepted = self.editor.update(cx, |editor, cx| {
            editor.route_overlay_action(Box::new(action), window, cx)
        });
        if accepted {
            cx.stop_propagation();
        }
    }
}

impl FileView {
    /// The narrowest the file can be shown: its editor fits 80 columns of
    /// text beside its line numbers.
    pub fn min_width(&self, window: &Window, cx: &App) -> Pixels {
        let theme = cx.theme();
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font(theme.mono_font_family.clone()));
        let column = text_system
            .advance(font_id, theme.mono_font_size, 'm')
            .map_or(theme.mono_font_size * 0.6, |size| size.width);
        // Room for the line numbers too, which widen as the file grows.
        let lines = self.editor.read(cx).value().lines().count().max(1);
        let digits = lines.to_string().len().max(3);
        (column * (MIN_COLUMNS + digits + GUTTER_EXTRA_COLUMNS) as f32).ceil()
    }
}

impl Render for FileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let min_width = self.min_width(window, cx);

        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;

        let header = h_flex()
            .flex_none()
            .gap_2()
            .pl_3()
            .pr_1()
            .py_1()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_medium()
                    .child(self.title.clone()),
            )
            .when(self.dirty, |header| {
                header.child(
                    div()
                        .id("unsaved-changes")
                        .flex_none()
                        .size_2()
                        .rounded_full()
                        .bg(muted),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("save-file")
                    .ghost()
                    .small()
                    .label("Save")
                    .tooltip(format!("Save ({SAVE_SHORTCUT})"))
                    .disabled(!self.dirty)
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
            .child(
                Button::new("close-file")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Close file")
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            );

        let view = v_flex()
            .id("file-view")
            .key_context(CONTEXT)
            .size_full()
            .min_w(min_width)
            .on_action(cx.listener(|this, _: &SaveFile, window, cx| this.save(window, cx)))
            .capture_action(cx.listener(Self::route_enter_to_menus))
            .child(header)
            .when_some(self.save_error.clone(), |view, error| {
                view.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_1()
                        .border_b_1()
                        .border_color(border)
                        .text_color(danger)
                        .child(error),
                )
            })
            .child(div().flex_1().min_h_0().map(|body| {
                match &self.error {
                    Some(error) => body.p_3().text_color(muted).child(error.clone()),
                    // Read-only until the file's text is in, so nothing typed
                    // before is lost.
                    None => body.child(
                        Editor::new(&self.editor)
                            .readonly(self.saved.is_none())
                            .bordered(false)
                            .rounded_none()
                            .size_full(),
                    ),
                }
            }));
        // Lets UI tests find the file; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(view)
    }
}

/// A Piton file open in `piton lsp`; closed there when dropped.
struct Document {
    client: Arc<LspClient>,
    /// Canonical, as the client keys diagnostics.
    path: PathBuf,
    uri: String,
    sent: Mutex<Sent>,
    /// Completions from the last request that carry an import.
    offered: Mutex<Vec<OfferedCompletion>>,
}

/// The version and text last sent to the server.
struct Sent {
    version: i32,
    text: String,
}

struct OfferedCompletion {
    /// Byte offset in the file where the completion inserts `new_text`.
    start: usize,
    new_text: String,
    imports: Vec<TextEdit>,
}

impl Document {
    fn open(client: Arc<LspClient>, path: &Path, text: &str) -> Self {
        let uri = piton_lsp::file_uri(path);
        client
            .notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": { "uri": uri, "languageId": "piton", "version": 1, "text": text },
                }),
            )
            .ok();
        Self {
            client,
            path: piton_lsp::canonical(path),
            uri,
            sent: Mutex::new(Sent {
                version: 1,
                text: text.to_string(),
            }),
            offered: Mutex::default(),
        }
    }

    /// Sends `text` to the server, unless it is what the server already has.
    fn sync(&self, text: &str) {
        let mut sent = self.sent.lock().unwrap();
        if sent.text == text {
            return;
        }
        sent.version += 1;
        sent.text = text.to_string();
        self.client
            .notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": self.uri, "version": sent.version },
                    "contentChanges": [{ "text": text }],
                }),
            )
            .ok();
    }

    fn did_save(&self) {
        self.client
            .notify(
                "textDocument/didSave",
                json!({ "textDocument": { "uri": self.uri } }),
            )
            .ok();
    }

    /// `textDocument` and `position` params for byte `offset` of `text`.
    fn at(&self, text: &str, offset: usize) -> Value {
        json!({
            "textDocument": { "uri": self.uri },
            "position": piton_lsp::lsp_position(text, offset),
        })
    }

    /// The import edits of an offered completion now in `text` where it was
    /// offered, which the editor does not apply itself; last first, so each
    /// applies before the positions of the rest move.
    fn take_accepted_imports(&self, text: &str) -> Vec<TextEdit> {
        let mut offered = self.offered.lock().unwrap();
        let Some(ix) = offered.iter().position(|completion| {
            piton_lsp::is_inserted_at(text, completion.start, &completion.new_text)
        }) else {
            return Vec::new();
        };
        let mut imports = offered.swap_remove(ix).imports;
        offered.clear();
        imports.sort_by(|a, b| b.range.start.cmp(&a.range.start));
        imports
    }

    /// The edits in `edit` to this file, last first.
    fn edits_in(&self, edit: Option<WorkspaceEdit>) -> Vec<TextEdit> {
        let Some(edit) = edit else {
            return Vec::new();
        };
        let is_this_file = |uri: &lsp_types::Uri| {
            piton_lsp::uri_path(uri.as_str())
                .is_some_and(|path| piton_lsp::canonical(&path) == self.path)
        };
        let mut edits: Vec<TextEdit> = edit
            .changes
            .into_iter()
            .flatten()
            .filter(|(uri, _)| is_this_file(uri))
            .flat_map(|(_, edits)| edits)
            .collect();
        if let Some(DocumentChanges::Edits(changes)) = edit.document_changes {
            edits.extend(
                changes
                    .into_iter()
                    .filter(|change| is_this_file(&change.text_document.uri))
                    .flat_map(|change| change.edits)
                    .map(|edit| match edit {
                        OneOf::Left(edit) => edit,
                        OneOf::Right(annotated) => annotated.text_edit,
                    }),
            );
        }
        edits.sort_by(|a, b| b.range.start.cmp(&a.range.start));
        edits
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        self.client
            .notify(
                "textDocument/didClose",
                json!({ "textDocument": { "uri": self.uri } }),
            )
            .ok();
    }
}

/// Language support for the file open in the editor, from `piton lsp`.
struct PitonFile {
    document: Arc<Document>,
}

impl CompletionProvider for PitonFile {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let text = text.to_string();
        let document = self.document.clone();
        // The editor asks before it reports the edit that prompted it.
        document.sync(&text);
        cx.background_spawn(async move {
            let result = document
                .client
                .request("textDocument/completion", document.at(&text, offset))?;
            let items =
                piton_lsp::narrow_completions(piton_lsp::completion_items(result)?, &text, offset);

            *document.offered.lock().unwrap() = items
                .iter()
                .filter_map(|item| {
                    let Some(CompletionTextEdit::Edit(edit)) = &item.text_edit else {
                        return None;
                    };
                    let imports = item
                        .additional_text_edits
                        .clone()
                        .filter(|edits| !edits.is_empty())?;
                    Some(OfferedCompletion {
                        start: piton_lsp::byte_offset(&text, edit.range.start),
                        new_text: edit.new_text.clone(),
                        imports,
                    })
                })
                .collect();
            Ok(CompletionResponse::Array(items))
        })
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '{' | '.' | '$' | '@' | ':' | '_'))
    }
}

impl HoverProvider for PitonFile {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let text = text.to_string();
        let document = self.document.clone();
        document.sync(&text);
        cx.background_spawn(async move {
            let result = document
                .client
                .request("textDocument/hover", document.at(&text, offset))?;
            Ok(serde_json::from_value(result)?)
        })
    }
}

impl DefinitionProvider for PitonFile {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<LocationLink>>> {
        let text = text.to_string();
        let document = self.document.clone();
        document.sync(&text);
        cx.background_spawn(async move {
            let result = document
                .client
                .request("textDocument/definition", document.at(&text, offset))?;
            let link = |location: Location| LocationLink {
                origin_selection_range: None,
                target_uri: location.uri,
                target_range: location.range,
                target_selection_range: location.range,
            };
            Ok(match serde_json::from_value(result)? {
                None => Vec::new(),
                Some(GotoDefinitionResponse::Scalar(location)) => vec![link(location)],
                Some(GotoDefinitionResponse::Array(locations)) => {
                    locations.into_iter().map(link).collect()
                }
                Some(GotoDefinitionResponse::Link(links)) => links,
            })
        })
    }
}

impl CodeActionProvider for PitonFile {
    fn id(&self) -> SharedString {
        "piton-lsp".into()
    }

    fn code_actions(
        &self,
        state: Entity<EditorState>,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeAction>>> {
        let text = state.read(cx).value().to_string();
        let document = self.document.clone();
        document.sync(&text);
        let start = piton_lsp::lsp_position(&text, range.start);
        let end = piton_lsp::lsp_position(&text, range.end);
        cx.background_spawn(async move {
            // What is diagnosed at the selection, for the fixes to it.
            let diagnostics: Vec<_> = document
                .client
                .diagnostics(&document.path)
                .into_iter()
                .filter(|diagnostic| diagnostic.range.start <= end && start <= diagnostic.range.end)
                .collect();
            let result = document.client.request(
                "textDocument/codeAction",
                json!({
                    "textDocument": { "uri": document.uri },
                    "range": { "start": start, "end": end },
                    "context": { "diagnostics": diagnostics },
                }),
            )?;
            let actions: Option<Vec<CodeActionOrCommand>> = serde_json::from_value(result)?;
            Ok(actions
                .into_iter()
                .flatten()
                .filter_map(|action| match action {
                    CodeActionOrCommand::CodeAction(action) => Some(action),
                    CodeActionOrCommand::Command(_) => None,
                })
                .collect())
        })
    }

    fn perform_code_action(
        &self,
        state: Entity<EditorState>,
        action: CodeAction,
        _: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let edits = self.document.edits_in(action.edit);
        state.update(cx, |editor, cx| editor.apply_lsp_edits(&edits, window, cx));
        Task::ready(Ok(()))
    }
}

/// Opens a definition in another file through `file`'s [`OpenDefinition`];
/// one in the file itself is left to the editor, which moves to it.
fn show_document(document_path: PathBuf, file: WeakEntity<FileView>) -> ShowDocumentHandler {
    Rc::new(move |params, _, cx| {
        let Some(path) = piton_lsp::uri_path(params.uri.as_str()) else {
            return false;
        };
        if piton_lsp::canonical(&path) == document_path {
            return false;
        }
        let position = params
            .selection
            .map_or_else(Position::default, |range| range.start);
        file.update(cx, |_, cx| cx.emit(OpenDefinition { path, position }))
            .is_ok()
    })
}

/// `text` in Piton's canonical formatting, from `piton format` run in
/// `project_dir`; an error if it could not be formatted.
pub(crate) fn format_piton(text: &str, project_dir: &Path) -> anyhow::Result<String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let mut child = Command::new("piton")
        .args(["format", "-"])
        .current_dir(project_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Written from its own thread, so a large file can't fill both pipes.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdin"))?;
    let input = text.to_string();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output()?;
    writer.join().ok();
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let formatted = String::from_utf8(output.stdout)?;
    if formatted.trim().is_empty() && !text.trim().is_empty() {
        anyhow::bail!("piton format printed nothing");
    }
    Ok(formatted)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::time::Duration;

    use gpui_kit::component::{Root, WindowExt as _};
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, Focusable as _, TestAppContext};
    use lsp_types::Position;

    use super::{CloseFile, FileView};
    use crate::piton_lsp;
    use crate::piton_syntax;
    use crate::project_directory::ProjectDirectory;
    use crate::project_lsp::ProjectLsp;

    const TIMEOUT: Duration = Duration::from_secs(10);

    #[cfg(target_os = "macos")]
    const SAVE: &str = "cmd-s";
    #[cfg(not(target_os = "macos"))]
    const SAVE: &str = "ctrl-s";

    fn init(cx: &mut TestAppContext, project: Option<&Path>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            super::bind_keys(cx);
            ProjectDirectory::init(cx);
            if let Some(project) = project {
                ProjectDirectory::set(project.to_path_buf(), cx);
                ProjectLsp::init(cx);
            }
        });
    }

    fn open(
        cx: &mut TestAppContext,
        path: &Path,
        position: Option<Position>,
    ) -> (Entity<FileView>, AnyWindowHandle) {
        let mut view = None;
        let window = cx.add_window(|window, cx| {
            let file_view = cx.new(|cx| FileView::new(path.to_path_buf(), position, window, cx));
            view = Some(file_view.clone());
            Root::new(file_view, window, cx)
        });
        (view.unwrap(), window.into())
    }

    fn temp_file(name: &str, text: &str) -> PathBuf {
        let file = std::env::temp_dir().join(format!("suspense-{name}-{}.txt", std::process::id()));
        std::fs::write(&file, text).unwrap();
        file
    }

    fn type_keys(cx: &mut TestAppContext, handle: AnyWindowHandle, text: &str) {
        for key in text.chars() {
            cx.update_window(handle, |_, window, cx| window.input(&key.to_string(), cx))
                .unwrap();
            cx.run_until_parked();
        }
    }

    /// Typing edits the file, marking it unsaved, and Ctrl/Cmd+S writes it.
    #[gpui_kit::test]
    async fn edits_and_saves_the_file(cx: &mut TestAppContext) {
        const TEXT: &str = "# Notes\n";
        let file = temp_file("file-view-save", TEXT);
        init(cx, None);
        let (view, handle) = open(cx, &file, None);

        cx.wait_for(handle, TIMEOUT, |_, cx| {
            view.read(cx).editor.read(cx).value().as_ref() == TEXT
        })
        .await;
        assert!(view.read_with(cx, |view, _| !view.is_dirty()));

        cx.update_window(handle, |_, window, cx| {
            let focus = view.read(cx).editor.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        })
        .unwrap();
        type_keys(cx, handle, "Hi ");
        cx.update(|cx| {
            assert_eq!(
                view.read(cx).editor.read(cx).value().as_ref(),
                "Hi # Notes\n"
            );
            assert!(view.read(cx).is_dirty());
        });
        assert_eq!(std::fs::read_to_string(&file).unwrap(), TEXT);

        cx.update_window(handle, |_, window, cx| window.press(SAVE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| !view.read(cx).is_dirty())
            .await;
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "Hi # Notes\n");

        std::fs::remove_file(&file).ok();
    }

    /// Saving a Piton file formats it with `piton format`, on disk and in
    /// the editor.
    #[gpui_kit::test]
    async fn saving_a_piton_file_formats_it(cx: &mut TestAppContext) {
        let file = std::env::temp_dir().join(format!(
            "suspense-file-view-format-{}.pi",
            std::process::id()
        ));
        std::fs::write(&file, "anchor A:\n    x: 1\n").unwrap();
        init(cx, None);
        let (view, handle) = open(cx, &file, None);
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            view.read(cx).editor.read(cx).value().as_ref() == "anchor A:\n    x: 1\n"
        })
        .await;

        cx.update_window(handle, |_, window, cx| {
            let focus = view.read(cx).editor.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        })
        .unwrap();
        type_keys(cx, handle, "anchor   B:\n");
        cx.update_window(handle, |_, window, cx| window.press(SAVE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            let view = view.read(cx);
            !view.is_dirty() && view.editor.read(cx).value().starts_with("anchor B:")
        })
        .await;
        let saved = std::fs::read_to_string(&file).unwrap();
        assert!(saved.starts_with("anchor B:"), "{saved}");
        assert_eq!(
            view.read_with(cx, |view, cx| view.editor.read(cx).value().to_string()),
            saved
        );

        std::fs::remove_file(&file).ok();
    }

    /// The file is never shown narrower than 80 columns of its editor's text,
    /// even in a narrow window.
    #[gpui_kit::test]
    async fn editor_is_at_least_80_columns_wide(cx: &mut TestAppContext) {
        let file = temp_file("file-view-width", "short\n");
        init(cx, None);
        let (view, handle) = open(cx, &file, None);
        cx.update_window(handle, |_, window, cx| {
            window.resize(gpui_kit::size(gpui_kit::px(300.), gpui_kit::px(400.)));
            let _ = cx;
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            let min = view.read(cx).min_width(window, cx);
            let theme = gpui_kit::component::ActiveTheme::theme(cx);
            let font_id = window
                .text_system()
                .resolve_font(&gpui_kit::font(theme.mono_font_family.clone()));
            let column = window
                .text_system()
                .advance(font_id, theme.mono_font_size, 'm')
                .unwrap()
                .width;
            assert!(
                min >= column * 80.,
                "{min:?} is under 80 columns of {column:?}"
            );
            let shown = window.find("file-view").bounds().size.width;
            assert!(shown >= min, "shown {shown:?} narrower than {min:?}");
        })
        .unwrap();
        std::fs::remove_file(&file).ok();
    }

    /// Closing a file with unsaved changes asks first, rather than closing.
    #[gpui_kit::test]
    async fn closing_unsaved_changes_asks_first(cx: &mut TestAppContext) {
        let file = temp_file("file-view-close", "text\n");
        init(cx, None);
        let (view, handle) = open(cx, &file, None);
        let closed = Rc::new(Cell::new(false));
        let _subscription = cx.update(|cx| {
            let closed = closed.clone();
            cx.subscribe(&view, move |_, _: &CloseFile, _| closed.set(true))
        });

        cx.wait_for(handle, TIMEOUT, |_, cx| view.read(cx).saved.is_some())
            .await;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| view.focus_editor(window, cx));
        })
        .unwrap();
        type_keys(cx, handle, "more ");

        cx.update_window(handle, |_, window, cx| window.click("close-file", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            assert!(
                window.has_active_dialog(cx),
                "no confirmation was asked for"
            );
        })
        .unwrap();
        assert!(!closed.get(), "the file closed with unsaved changes");

        std::fs::remove_file(&file).ok();
    }

    /// A spec file of this repository, opened with the cursor after `anchor`
    /// and `piton lsp` running over the project. Nothing is ever saved.
    async fn open_spec_file(
        cx: &mut TestAppContext,
        anchor: &str,
    ) -> (Entity<FileView>, AnyWindowHandle) {
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = project.join("spec/scope/editor/index.pi");
        let source = std::fs::read_to_string(&path).unwrap();
        let cursor = piton_lsp::lsp_position(&source, source.find(anchor).unwrap() + anchor.len());

        init(cx, Some(&project));
        let (view, handle) = open(cx, &path, Some(cursor));
        cx.wait_for(handle, TIMEOUT, |_, cx| view.read(cx).document.is_some())
            .await;
        (view, handle)
    }

    /// Typing a reference in a Piton file completes it from `piton lsp`, and
    /// accepting the completion with Enter also imports the name.
    #[gpui_kit::test]
    async fn completing_a_reference_imports_it(cx: &mut TestAppContext) {
        let (view, handle) = open_spec_file(cx, "support for Piton files.").await;
        let editor = view.read_with(cx, |view, _| view.editor.clone());

        type_keys(cx, handle, " See @{ApplicationSc");
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            let menu = editor.read(cx).completion_menu_state();
            menu.open
                && menu
                    .items
                    .first()
                    .is_some_and(|item| item.label == "ApplicationScope")
        })
        .await;

        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            editor
                .read(cx)
                .value()
                .contains("from ../application import ApplicationScope\n")
        })
        .await;
        let text = editor.read_with(cx, |editor, _| editor.value().to_string());
        assert!(
            text.contains("support for Piton files. See @{ApplicationScope}\n"),
            "{text}"
        );
    }

    /// An unknown reference typed into a Piton file is diagnosed by
    /// `piton lsp` and shown in the editor.
    #[gpui_kit::test]
    async fn unknown_reference_is_diagnosed(cx: &mut TestAppContext) {
        let (view, handle) = open_spec_file(cx, "support for Piton files.").await;
        let editor = view.read_with(cx, |view, _| view.editor.clone());

        type_keys(cx, handle, " See @{NoSuchScope");
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            view.read(cx)
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("NoSuchScope"))
                && editor
                    .read(cx)
                    .diagnostics()
                    .is_some_and(|set| !set.is_empty())
        })
        .await;
    }
}
