//! A file opened from the project tree, in an editor: syntax highlighted,
//! saved with Ctrl/Cmd+S, and for Piton files backed by the project's
//! `piton lsp` for completions, hover, definitions, diagnostics and quick
//! fixes. A header above it shows its path in the project, whether it has
//! unsaved changes, and save and close buttons.
//!
//! It watches its file on disk: edited there, the file reloads, unless it has
//! unsaved changes, when it asks what to do through [`ChangedOnDisk`], and
//! can be merged in a [`DiffView`]; renamed there, the editor follows it.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::input::{
    Backspace, CodeActionProvider, CompletionProvider, Copy, Cut, DefinitionProvider, Editor,
    EditorState, Enter, HoverProvider, InputEvent, MoveHome, Paste, Rope, RopeExt as _,
    SelectToStartOfLine, ShowDocumentHandler, TabSize,
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

use crate::diff_view::{ApplyMerge, CancelMerge, DiffView};
use crate::disk_watch;
use crate::piton_lsp::{self, LspClient};
use crate::piton_syntax;
use crate::project_directory::ProjectDirectory;
use crate::project_lsp::ProjectLsp;
use crate::selection_popover::{SelectionAction, selection_popover};

actions!(file_view, [SaveFile]);

const CONTEXT: &str = "FileView";

/// How often what the server published is collected.
const DIAGNOSTICS_INTERVAL: Duration = Duration::from_millis(100);

/// How often what happened to the file on disk is collected.
const DISK_INTERVAL: Duration = Duration::from_millis(100);

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

/// Emitted to attach text selected in the editor to the prompt.
pub struct SendToPrompt(pub String);

/// Emitted when the file was edited on disk while it has unsaved changes,
/// to ask whether to keep them, reload, or merge.
pub struct ChangedOnDisk;

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

/// How many spaces a tab stop is.
pub const TAB_SIZE: usize = 4;

/// The column the ruler marks: the width a line should keep within.
pub const RULER_COLUMN: usize = 80;

/// How much black is laid over the well past the ruler, in dark and light
/// mode.
const PAST_RULER_SHADE: (f32, f32) = (0.25, 0.05);

pub struct FileView {
    path: PathBuf,
    /// The language the file is highlighted in, as [`language_for`] names it.
    language: String,
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
    /// Where the popover for selected text shows, while it does: where the
    /// drag that selected the text ended.
    selection_popover: Option<Point<Pixels>>,
    /// Whether it is being written, when what is on disk is its own doing.
    saving: bool,
    /// Whether the disk changed while it was being written, to look again
    /// once it is.
    recheck: bool,
    /// Whether it was edited on disk with unsaved changes here, and that is
    /// waiting to be answered.
    conflict: bool,
    /// The merge shown in place of the editor, while there is one.
    merge: Option<(Entity<DiffView>, Vec<Subscription>)>,
    _load: Task<()>,
    _save: Task<()>,
    _check_disk: Task<()>,
    _watch_disk: Task<()>,
    _watch_diagnostics: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CloseFile> for FileView {}
impl EventEmitter<SendToPrompt> for FileView {}
impl EventEmitter<OpenDefinition> for FileView {}
impl EventEmitter<ChangedOnDisk> for FileView {}

/// Where `offset` in `before` lands in `after`, the same text reformatted,
/// which changes only whitespace: against the same character it was
/// against, or else just after the same one, then as many line breaks on
/// and as many spaces along as the formatted text allows.
fn offset_after_formatting(before: &str, after: &str, offset: usize) -> usize {
    let offset = offset.min(before.len());
    let head = &before[..offset];
    let solid = head.chars().filter(|c| !c.is_whitespace()).count();
    // What lay between the last character and the cursor.
    let gap = &head[head.trim_end().len()..];
    let breaks = gap.matches('\n').count();
    let column = gap
        .rsplit('\n')
        .next()
        .map_or(0, |tail| tail.chars().count());

    let mut at = 0;
    let mut seen = 0;
    for (ix, c) in after.char_indices() {
        if seen == solid {
            break;
        }
        if !c.is_whitespace() {
            seen += 1;
        }
        at = ix + c.len_utf8();
    }
    if seen < solid {
        return after.len();
    }
    // Against a character, it stays against it.
    if before[offset..].starts_with(|c: char| !c.is_whitespace()) {
        return after[at..]
            .find(|c: char| !c.is_whitespace())
            .map_or(after.len(), |next| at + next);
    }
    // Then as many line breaks on, where the formatted text still has them.
    for _ in 0..breaks {
        match after[at..].find('\n') {
            Some(newline) if after[at..at + newline].trim().is_empty() => at += newline + 1,
            _ => break,
        }
    }
    // And as far along the line as it was, without passing its next character.
    let room = after[at..]
        .char_indices()
        .take_while(|(_, c)| *c == ' ' || *c == '\t')
        .take(column)
        .last()
        .map_or(0, |(ix, c)| ix + c.len_utf8());
    at + room
}

/// Where `offset` in `before` lands in `after`, the same file as edited
/// elsewhere: in a line both share, beside the same characters; in a line
/// that changed, as far along the line that took its place as the line
/// allows.
fn offset_after_edit(before: &str, after: &str, offset: usize) -> usize {
    let offset = offset.min(before.len());
    let starts = |text: &str| {
        std::iter::once(0)
            .chain(text.match_indices('\n').map(|(ix, _)| ix + 1))
            .collect::<Vec<_>>()
    };
    let (old_starts, new_starts) = (starts(before), starts(after));
    let line = old_starts.partition_point(|start| *start <= offset) - 1;
    let column = offset - old_starts[line];
    let diff = similar::TextDiff::from_lines(before, after);
    for op in diff.ops() {
        let (old, new) = (op.old_range(), op.new_range());
        if !old.contains(&line) {
            continue;
        }
        if new.is_empty() {
            // Its line is gone: to where the lines after it now start.
            return new_starts.get(new.start).copied().unwrap_or(after.len());
        }
        let new_line = (new.start + (line - old.start)).min(new.end - 1);
        let start = new_starts[new_line];
        let end = after[start..]
            .find('\n')
            .map_or(after.len(), |end| start + end);
        let mut at = (start + column).min(end);
        while !after.is_char_boundary(at) {
            at -= 1;
        }
        return at;
    }
    after.len()
}

/// Text as the editor holds it, with Unix line endings.
fn normalized(text: String) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n")
    } else {
        text
    }
}

/// The file at `path` as the header names it: within the project, or in
/// full outside of one.
fn title_for(path: &Path, cx: &App) -> SharedString {
    match ProjectDirectory::get(cx) {
        Some(root) => path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string()
            .into(),
        None => path.display().to_string().into(),
    }
}

impl FileView {
    /// Opens the file at `path`, with the cursor at `position` if given.
    pub fn new(
        path: PathBuf,
        position: Option<Position>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = title_for(&path, cx);
        let language = language_for(&path);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language.clone())
                .tab_size(TabSize {
                    tab_size: TAB_SIZE,
                    hard_tabs: false,
                })
                .soft_wrap(false)
                .placeholder("Loading…")
        });

        let subscriptions = vec![
            cx.subscribe_in(
                &editor,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        // What was selected has changed under the popover.
                        this.selection_popover = None;
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

        let mut this = Self {
            path,
            language,
            title,
            editor,
            error: None,
            save_error: None,
            saved: None,
            dirty: false,
            document: None,
            diagnostics: Vec::new(),
            selection_popover: None,
            saving: false,
            recheck: false,
            conflict: false,
            merge: None,
            _load: load,
            _save: Task::ready(()),
            _check_disk: Task::ready(()),
            _watch_disk: Task::ready(()),
            _watch_diagnostics: Task::ready(()),
            _subscriptions: subscriptions,
        };
        this._watch_disk = this.watch_disk(window, cx);
        this
    }

    /// Collects what the watcher reports about the file's folders, and acts
    /// on what happened to the file.
    fn watch_disk(&self, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        let events = disk_watch::subscribe(&self.path, ProjectDirectory::get(cx).as_deref());
        // Collected on a timer rather than awaited: the watcher reports from
        // its own thread, which must not wake app tasks (tests forbid it).
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(DISK_INTERVAL).await;
                let batch: Vec<_> = events.try_iter().collect();
                if batch.is_empty() {
                    continue;
                }
                let updated = this.update_in(cx, |this, window, cx| {
                    this.on_disk_events(&batch, window, cx)
                });
                if updated.is_err() {
                    break;
                }
            }
        })
    }

    /// Follows the file renamed on disk, and looks again at what it holds
    /// when it may have been written.
    fn on_disk_events(
        &mut self,
        events: &[notify::Event],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changes = disk_watch::changes(&self.path, events);
        if let Some(to) = changes.renamed {
            self.follow_rename(to, window, cx);
            // Renamed and edited at once, it is both.
            self.check_disk(window, cx);
            return;
        }
        if changes.vanished && !self.path.exists() {
            // Gone without saying where: renamed to a file that appeared with
            // just what it held, or else deleted, which leaves it as it is.
            let moved_to = self.saved.as_ref().and_then(|saved| {
                changes.appeared.iter().find(|path| {
                    path.is_file()
                        && std::fs::read_to_string(path)
                            .is_ok_and(|text| normalized(text) == *saved)
                })
            });
            if let Some(to) = moved_to.cloned() {
                self.follow_rename(to, window, cx);
            }
            return;
        }
        if changes.written || changes.vanished {
            self.check_disk(window, cx);
        }
    }

    /// Follows the file to `path`, where it was renamed or moved, in the same
    /// editor: its text, unsaved changes, cursor, and scrolling stay, and a
    /// Piton file is closed in the server under its old path and opened under
    /// its new one.
    pub fn follow_rename(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if path == self.path {
            return;
        }
        self.title = title_for(&path, cx);
        let language = language_for(&path);
        if language != self.language {
            self.editor.update(cx, |editor, cx| {
                editor.set_highlighter(language.clone(), cx)
            });
            self.language = language;
        }
        self.path = path;
        self.open_in_lsp(true, cx);
        self._watch_disk = self.watch_disk(window, cx);
        cx.notify();
    }

    /// Reads the file on disk again, and acts on how it differs from what
    /// was last read or written.
    fn check_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saved.is_none() || self.error.is_some() {
            return;
        }
        if self.saving {
            self.recheck = true;
            return;
        }
        let read = cx.background_spawn({
            let path = self.path.clone();
            async move { std::fs::read_to_string(path) }
        });
        self._check_disk = cx.spawn_in(window, async move |this, cx| {
            let Ok(text) = read.await else { return };
            this.update_in(cx, |this, window, cx| {
                this.disk_changed(normalized(text), window, cx)
            })
            .ok();
        });
    }

    /// The file on disk holds `text`: reloaded when there are no unsaved
    /// changes, and otherwise asked about.
    fn disk_changed(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            self.recheck = true;
            return;
        }
        if self.saved.as_ref().is_none_or(|saved| *saved == text) {
            return;
        }
        // Being asked or merged, the answer acts on the disk as it is then.
        if self.conflict || self.merge.is_some() {
            return;
        }
        if !self.dirty {
            return self.reload(text, window, cx);
        }
        // Changed on disk to just what is here: nothing is unsaved any more.
        if self.editor.read(cx).value().as_ref() == text {
            self.saved = Some(text);
            self.dirty = false;
            cx.notify();
            return;
        }
        self.conflict = true;
        cx.emit(ChangedOnDisk);
        cx.notify();
    }

    /// Shows `text` in place of what the editor holds, as the file now is
    /// on disk: the cursor beside the same characters, the view where it
    /// was, and nothing to undo back to.
    fn reload(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.set_text(text, window, cx);
        self.saved = Some(self.editor.read(cx).value().to_string());
        self.dirty = false;
        cx.notify();
    }

    /// Replaces the editor's text with `text`, keeping the cursor and the
    /// view, and tells the server.
    fn set_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.selection_popover = None;
        self.editor.update(cx, |editor, cx| {
            let before = editor.value().to_string();
            let selected = editor.selected_range();
            let scroll = editor.scroll_offset();
            editor.set_value(text.clone(), window, cx);
            let start = offset_after_edit(&before, &text, selected.start);
            let end = offset_after_edit(&before, &text, selected.end).max(start);
            editor.set_selected_range(start..end, cx);
            editor.set_scroll_offset(scroll, cx);
            editor.clear_hover_state(cx);
            editor.clear_diagnostic_popover(cx);
        });
        if let Some(document) = &self.document {
            document.sync(&self.editor.read(cx).value());
        }
        self.show_diagnostics(cx);
    }

    /// The file as it is on disk now, as the editor would hold it.
    fn read_disk(&self) -> Option<String> {
        std::fs::read_to_string(&self.path).ok().map(normalized)
    }

    /// Whether it was edited on disk with unsaved changes, and is waiting
    /// to be asked what to do.
    pub fn has_conflict(&self) -> bool {
        self.conflict
    }

    /// Keeps the unsaved changes over what is on disk now, so saving writes
    /// over it.
    pub fn keep_mine(&mut self, cx: &mut Context<Self>) {
        self.conflict = false;
        if let Some(disk) = self.read_disk() {
            self.dirty = self.editor.read(cx).value().as_ref() != disk;
            self.saved = Some(disk);
        }
        cx.notify();
    }

    /// Discards the unsaved changes, reloading the file as it is on disk now.
    pub fn reload_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.conflict = false;
        match self.read_disk() {
            Some(disk) => self.reload(disk, window, cx),
            None => cx.notify(),
        }
    }

    /// Shows a merge of the file on disk and the unsaved text in place of
    /// the editor.
    pub fn start_merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.conflict = false;
        self.selection_popover = None;
        let mine = self.editor.read(cx).value().to_string();
        let path = self.path.clone();
        let diff = cx.new(|cx| DiffView::merging(path, mine, cx));
        let subscriptions = vec![
            cx.subscribe_in(&diff, window, |this, _, ApplyMerge(text), window, cx| {
                this.apply_merge(text.clone(), window, cx)
            }),
            cx.subscribe_in(&diff, window, |this, _, _: &CancelMerge, window, cx| {
                this.end_merge(window, cx)
            }),
        ];
        diff.read(cx).focus_handle(cx).focus(window, cx);
        self.merge = Some((diff, subscriptions));
        cx.notify();
    }

    /// Puts the merged `text` in the editor, unsaved, against the file as it
    /// is on disk now.
    fn apply_merge(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(disk) = self.read_disk() {
            self.saved = Some(disk);
        }
        self.set_text(text, window, cx);
        self.dirty = self
            .saved
            .as_ref()
            .is_none_or(|saved| self.editor.read(cx).value().as_ref() != saved);
        self.merge = None;
        self.focus_editor(window, cx);
        cx.notify();
    }

    /// Shows the editor again with its text as it was, the unsaved changes
    /// kept over what is on disk.
    fn end_merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.merge = None;
        self.keep_mine(cx);
        self.focus_editor(window, cx);
    }

    /// Types `text` at the cursor, as the keyboard would.
    #[cfg(test)]
    pub fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |editor, cx| editor.insert(text.to_string(), window, cx));
    }

    /// The merge shown in place of the editor, while there is one.
    #[cfg(test)]
    pub fn merge_view(&self) -> Option<Entity<DiffView>> {
        self.merge.as_ref().map(|(diff, _)| diff.clone())
    }

    /// Shows a file that isn't on disk yet, starting with `text`, the cursor
    /// at `cursor`: unsaved from the start, and written, folders and all,
    /// when saved.
    pub fn unwritten(
        path: PathBuf,
        text: String,
        cursor: Position,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new(path, None, window, cx);
        // There is nothing to read: dropping the load stops it.
        this._load = Task::ready(());
        this.editor.update(cx, |editor, cx| {
            editor.set_value(text, window, cx);
            editor.set_cursor_position(cursor, window, cx);
        });
        // Nothing is saved yet, so everything in it is a change.
        this.saved = Some(String::new());
        this.dirty = true;
        this.attach_lsp(cx);
        this
    }

    /// The file shown.
    pub fn path(&self) -> &std::path::Path {
        &self.path
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

    /// Moves keyboard focus into the editor.
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
        // What the pointer's hover showed goes as soon as the text changes.
        self.editor.update(cx, |editor, cx| {
            editor.clear_hover_state(cx);
            editor.clear_diagnostic_popover(cx);
        });

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
        // What the disk shows until it is written is this save's own doing.
        self.saving = true;
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
                // A file not yet on disk may need its folders made.
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                // Saved here, it is kept even where a running task may not
                // change it.
                crate::mode_guard::saving(&path, text.as_bytes());
                std::fs::write(path, &text).map(|()| text)
            }
        });
        self._save = cx.spawn_in(window, async move |this, cx| {
            let written = write.await;
            this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match written {
                    Ok(text) => {
                        this.save_error = None;
                        // Shows the formatted text, unless the file was edited
                        // again while it saved.
                        if text != typed && this.editor.read(cx).value().as_ref() == typed {
                            // The cursor and the view stay where they were.
                            this.editor.update(cx, |editor, cx| {
                                let selected = editor.selected_range();
                                let scroll = editor.scroll_offset();
                                editor.set_value(text.clone(), window, cx);
                                let start = offset_after_formatting(&typed, &text, selected.start);
                                let end = offset_after_formatting(&typed, &text, selected.end);
                                editor.set_selected_range(start..end, cx);
                                editor.set_scroll_offset(scroll, cx);
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
                if std::mem::take(&mut this.recheck) {
                    this.check_disk(window, cx);
                }
                cx.notify();
            })
            .ok();
        });
    }

    #[cfg(test)]
    pub fn text(&self, cx: &App) -> SharedString {
        self.editor.read(cx).value()
    }

    /// Whether the editor has keyboard focus.
    #[cfg(test)]
    pub fn editor_focused(&self, window: &Window, cx: &App) -> bool {
        use gpui_kit::Focusable as _;
        self.editor.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Whether the popover for selected text is showing.
    #[cfg(test)]
    pub fn selection_popover_shown(&self) -> bool {
        self.selection_popover.is_some()
    }

    /// Does what `action` does in the editor, Cut, Copy, or Paste, with the
    /// editor focused, closing the popover.
    fn edit_selection(
        &mut self,
        action: Box<dyn Action>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selection_popover = None;
        let focus = self.editor.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        // Straight to the editor, as its own shortcut would be.
        focus.dispatch_action(action.as_ref(), window, cx);
        cx.notify();
    }

    /// Attaches the selected text to the prompt, closing the popover.
    fn send_selection_to_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selection_popover = None;
        let text = self.editor.read(cx).selected_text().to_string();
        if !text.is_empty() {
            cx.emit(SendToPrompt(text));
        }
        self.editor.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// The popover by text selected in the editor: Cut, Copy, Paste, and Send
    /// to Prompt. Pressing the mouse anywhere else closes it.
    fn render_selection_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let position = self.selection_popover?;
        // Cut and Paste change the file, which can't be edited until it's in.
        let editable = self.saved.is_some();
        let this = cx.entity().downgrade();
        let edit = |action: fn() -> Box<dyn Action>| {
            let this = this.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                this.update(cx, |this, cx| this.edit_selection(action(), window, cx))
                    .ok();
            }
        };
        let actions = vec![
            SelectionAction::new("cut", IconName::Scissors, "Cut", edit(|| Box::new(Cut)))
                .disabled(!editable),
            SelectionAction::new("copy", IconName::Copy, "Copy", edit(|| Box::new(Copy))),
            SelectionAction::new(
                "paste",
                IconName::ClipboardPaste,
                "Paste",
                edit(|| Box::new(Paste)),
            )
            .disabled(!editable),
            SelectionAction::new("send", IconName::Paperclip, "Send to Prompt", {
                let this = this.clone();
                move |_, window, cx| {
                    this.update(cx, |this, cx| this.send_selection_to_prompt(window, cx))
                        .ok();
                }
            }),
        ];
        Some(selection_popover(
            "editor-selection",
            position,
            actions,
            move |_, cx| {
                this.update(cx, |this, cx| {
                    this.selection_popover = None;
                    cx.notify();
                })
                .ok();
            },
            cx,
        ))
    }

    /// Moves the cursor to `position`, as when going to a definition in a
    /// file already open.
    pub fn go_to(&mut self, position: Position, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.set_cursor_position(position, window, cx)
        });
    }

    /// Closes the file, first asking whether to discard any unsaved changes.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.open_in_lsp(false, cx)
    }

    /// As [`Self::attach_lsp`], opening the file in the server afresh when
    /// `reopen`, as once it is renamed.
    fn open_in_lsp(&mut self, reopen: bool, cx: &mut Context<Self>) {
        let is_piton = self.path.extension().is_some_and(|ext| ext == "pi");
        // A file kept for a project not on screen isn't in the server, which
        // is the project on screen's, until its project is back.
        let on_screen = ProjectDirectory::get(cx).is_some_and(|dir| self.path.starts_with(dir));
        let client = ProjectLsp::get(cx).filter(|_| is_piton && on_screen && self.saved.is_some());
        let attached = self
            .document
            .as_ref()
            .map(|document| Arc::as_ptr(&document.client));
        if client.as_ref().map(Arc::as_ptr) == attached && !reopen {
            return;
        }

        let text = self.editor.read(cx).value();
        // Closed under its old path before it opens under its new one.
        self.document = None;
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
            if !action.shift && self.continues_lists() && self.continue_list(window, cx) {
                cx.stop_propagation();
            }
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
    /// Whether <Enter> on a list item starts the next: in prose, Markdown or
    /// Piton, rather than code, where a line starting with a dash is not one.
    fn continues_lists(&self) -> bool {
        matches!(self.language.as_str(), "" | "txt" | "md" | "markdown")
            || self.language == piton_syntax::LANGUAGE_NAME
    }

    /// <Enter> on a list item, with nothing selected: starts the next item,
    /// or ends the list on an empty one. Whether it did either.
    fn continue_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let editor = self.editor.read(cx);
        // Read-only until the file's text is in.
        if !editor.selected_range().is_empty() || self.saved.is_none() {
            return false;
        }
        let text = editor.text();
        let cursor = editor.cursor();
        let row = text.offset_to_position(cursor).line as usize;
        let start = text.line_start_offset(row);
        let line = text.slice_line(row).to_string();
        let Some(next) = list_continuation(&line, cursor - start) else {
            return false;
        };
        self.editor.update(cx, |editor, cx| match next {
            ListEnter::Next(item) => editor.insert(format!("\n{item}"), window, cx),
            ListEnter::End(indent) => {
                editor.set_selected_range(start..start + line.len(), cx);
                editor.replace(indent, window, cx);
            }
        });
        true
    }

    /// <Backspace> in a line's leading spaces: back to the tab stop before
    /// the cursor. Anything else is left to the editor.
    fn backspace_to_tab_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor.read(cx);
        if !editor.selected_range().is_empty() || self.saved.is_none() {
            return;
        }
        let text = editor.text();
        let cursor = editor.cursor();
        let row = text.offset_to_position(cursor).line as usize;
        let start = text.line_start_offset(row);
        let line = text.slice_line(row).to_string();
        let Some(width) = dedent_width(&line, cursor - start) else {
            return;
        };
        self.editor.update(cx, |editor, cx| {
            editor.set_selected_range(cursor - width..cursor, cx);
            editor.replace("", window, cx);
        });
        cx.stop_propagation();
    }

    /// <Home>, or <Shift+Home> when `select`: to the line's first character
    /// that isn't whitespace, or from there to the start of the line.
    fn smart_home(&mut self, select: bool, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        let editor = self.editor.read(cx);
        let text = editor.text();
        let cursor = editor.cursor();
        let row = text.offset_to_position(cursor).line as usize;
        let start = text.line_start_offset(row);
        let line = text.slice_line(row).to_string();
        let target = start + home_column(&line, cursor - start);
        let range = editor.selected_range();
        // The end of the selection the cursor isn't at stays put.
        let anchor = if range.start == cursor {
            range.end
        } else {
            range.start
        };
        self.editor.update(cx, |editor, cx| {
            if select {
                editor.set_selected_range(anchor..target, cx);
            } else {
                editor.set_selected_range(target..target, cx);
            }
        });
        cx.stop_propagation();
    }

    /// Paints the ruler at [`RULER_COLUMN`], and the darker background past
    /// it, from the editor's own layout, so they scroll sideways with the
    /// text. Drawn behind the text, whose editor has no background of its own.
    fn ruler(&self, cx: &App) -> impl IntoElement {
        let editor = self.editor.downgrade();
        let color = cx.theme().border;
        let (dark, light) = PAST_RULER_SHADE;
        let shade = gpui_kit::black().opacity(if cx.theme().is_dark() { dark } else { light });
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                let Some(editor) = editor.upgrade() else {
                    return;
                };
                let editor = editor.read(cx);
                let Some(row) = editor.visible_row_range().map(|rows| rows.start) else {
                    return;
                };
                let text = editor.text();
                let row = row.min(text.lines_len().saturating_sub(1));
                let start = text.line_start_offset(row);
                let Some(line) = editor.range_to_bounds(&(start..start)) else {
                    return;
                };
                let theme = cx.theme();
                let text_system = window.text_system();
                let font_id = text_system.resolve_font(&font(theme.mono_font_family.clone()));
                let column = text_system
                    .advance(font_id, theme.mono_font_size, 'm')
                    .map_or(theme.mono_font_size * 0.6, |size| size.width);
                let x = line.left() + column * RULER_COLUMN as f32;
                // Scrolled past, it would lie over the line numbers.
                let text_left = line.left() - editor.scroll_offset().x;
                if x > bounds.right() {
                    return;
                }
                // The shade runs from the ruler, or from the text's left edge
                // once the ruler is scrolled past, to the right edge.
                let shade_left = x.max(text_left).round();
                window.paint_quad(fill(
                    Bounds::from_corners(
                        point(shade_left, bounds.top()),
                        point(bounds.right(), bounds.bottom()),
                    ),
                    shade,
                ));
                if x < text_left {
                    return;
                }
                window.paint_quad(fill(
                    Bounds::new(
                        point(x.round(), bounds.top()),
                        size(px(1.), bounds.size.height),
                    ),
                    color,
                ));
            },
        )
        .absolute()
        .inset_0()
    }

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

/// The file's header: as tall as the body's tab bar, and its buttons.
const HEADER_HEIGHT: Pixels = px(32.);
const HEADER_BUTTON: Pixels = px(24.);

/// How far the editor is set in so its content, inside its own 10 pixels of
/// padding, starts 12 pixels from the tab's left edge.
const TEXT_NUDGE: Pixels = px(2.);

impl Render for FileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let min_width = self.min_width(window, cx);

        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;

        // As tall as the body's tab bar, its contents centred up and down,
        // 12 pixels in at its left and 8 at its right, its line the only one
        // in the tab.
        // Lets UI tests find the header; inert in normal builds.
        let header = gpui_kit::TestSupportExt::test_support(h_flex().id("file-header"))
            .flex_none()
            .h(HEADER_HEIGHT)
            .items_center()
            .pl_3()
            .pr_2()
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
                        .ml(px(6.))
                        .flex_none()
                        .size_2()
                        .rounded_full()
                        .bg(muted),
                )
            })
            .child(div().flex_1().min_w_2())
            .child(
                Button::new("save-file")
                    .ghost()
                    .xsmall()
                    .h(HEADER_BUTTON)
                    .label("Save")
                    .tooltip(format!("Save ({SAVE_SHORTCUT})"))
                    .disabled(!self.dirty)
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
            .child(
                Button::new("close-file")
                    .ghost()
                    .xsmall()
                    .size(HEADER_BUTTON)
                    .ml_1()
                    .icon(IconName::X)
                    .tooltip("Close file")
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            );

        // Merging, the merge shows in place of the editor.
        if let Some((merge, _)) = &self.merge {
            let view = v_flex()
                .id("file-view")
                .size_full()
                .min_w(min_width)
                .child(merge.clone());
            return gpui_kit::TestSupportExt::test_support(view);
        }

        let view = v_flex()
            .id("file-view")
            .key_context(CONTEXT)
            .size_full()
            .min_w(min_width)
            .on_action(cx.listener(|this, _: &SaveFile, window, cx| this.save(window, cx)))
            .capture_action(cx.listener(Self::route_enter_to_menus))
            .capture_action(
                cx.listener(|this, _: &Backspace, window, cx| {
                    this.backspace_to_tab_stop(window, cx)
                }),
            )
            .capture_action(
                cx.listener(|this, _: &MoveHome, window, cx| this.smart_home(false, window, cx)),
            )
            .capture_action(cx.listener(|this, _: &SelectToStartOfLine, window, cx| {
                this.smart_home(true, window, cx)
            }))
            .child(header)
            .when_some(self.save_error.clone(), |view, error| {
                view.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(border)
                        .text_color(danger)
                        .child(error),
                )
            })
            .child(div().flex_1().min_h_0().map(|body| {
                match &self.error {
                    Some(error) => body.px_3().py_2().text_color(muted).child(error.clone()),
                    // Read-only until the file's text is in, so nothing typed
                    // before is lost.
                    None => body
                        .relative()
                        // A drag that selects some of the text offers to cut,
                        // copy, paste over, or send it to the prompt, once the
                        // selection has settled.
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|_, event: &MouseUpEvent, window, cx| {
                                let position = event.position;
                                cx.defer_in(window, move |this, _, cx| {
                                    let selected =
                                        !this.editor.read(cx).selected_range().is_empty();
                                    if selected {
                                        this.selection_popover = Some(position);
                                        cx.notify();
                                    }
                                });
                            }),
                        )
                        // The well is drawn here, beneath the ruler, so the
                        // ruler and its shade sit behind the text.
                        .bg(crate::theme::color(crate::theme::palette(cx).well))
                        .child(self.ruler(cx))
                        // The editor pads its content 10 pixels at its left;
                        // set in 2 more, its line numbers start 12 pixels in,
                        // under the path in the header.
                        .child(
                            gpui_kit::TestSupportExt::test_support(div().id("file-text"))
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(TEXT_NUDGE)
                                .right_0()
                                .child(
                                    Editor::new(&self.editor)
                                        .readonly(self.saved.is_none())
                                        .appearance(false)
                                        .rounded_none()
                                        .size_full(),
                                ),
                        ),
                }
            }))
            .children(self.render_selection_popover(cx));
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

/// The language `path` is highlighted as, by its name or extension: Piton, or
/// any language gpui-kit has a grammar for. One it does not know, or has no
/// grammar for, is plain text.
pub(crate) fn language_for(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    // Files known by their whole name rather than an extension.
    let by_name = match name {
        "Makefile" | "makefile" | "GNUmakefile" => Some("make"),
        "CMakeLists.txt" => Some("cmake"),
        "Cargo.lock" | "Pipfile" => Some("toml"),
        "Gemfile" | "Rakefile" | "Podfile" | "Brewfile" => Some("ruby"),
        ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" | ".zprofile" | ".envrc" => {
            Some("bash")
        }
        _ => None,
    };
    if let Some(language) = by_name {
        return language.to_string();
    }
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return String::new();
    };
    let ext = ext.to_ascii_lowercase();
    // Extensions of languages gpui-kit knows under another name.
    let language = match ext.as_str() {
        "pi" => piton_syntax::LANGUAGE_NAME,
        "h" => "c",
        "hpp" | "hh" | "hxx" | "cc" | "cxx" | "ino" => "cpp",
        "jsx" | "mjs" | "cjs" => "javascript",
        "mts" | "cts" => "typescript",
        "htm" | "xhtml" => "html",
        "exs" | "heex" => "elixir",
        "zsh" | "ksh" => "bash",
        "json5" | "jsonl" | "geojson" => "json",
        "mk" => "make",
        "less" | "sass" => "css",
        "kts" | "ktm" => "kotlin",
        "gql" => "graphql",
        "markdown" | "mdown" => "markdown",
        "yml" => "yaml",
        ext => ext,
    };
    language.to_string()
}

/// How many spaces <Backspace> with the cursor `at` a byte of `line` takes
/// back to the tab stop before it, when all before the cursor is spaces;
/// `None` when it should delete as ever.
fn dedent_width(line: &str, at: usize) -> Option<usize> {
    let before = line.get(..at)?;
    if at == 0 || !before.bytes().all(|b| b == b' ') {
        return None;
    }
    let width = match at % TAB_SIZE {
        0 => TAB_SIZE,
        rest => rest,
    };
    (width > 1).then_some(width)
}

/// Where <Home> goes on `line`, with the cursor `at` a byte in it: to its
/// first character that isn't whitespace, or, from there or on a line of only
/// whitespace, to its start.
fn home_column(line: &str, at: usize) -> usize {
    let line = line.trim_end_matches(['\n', '\r']);
    let first = line.len() - line.trim_start().len();
    if at == first || first == line.len() {
        0
    } else {
        first
    }
}

/// What <Enter> does on a list item.
#[derive(Debug, PartialEq)]
enum ListEnter {
    /// Starts a new line holding the next item's indentation and marker.
    Next(String),
    /// Ends the list: the empty item's line becomes this indentation alone.
    End(String),
}

/// What <Enter> with the cursor `at` a byte of `line` does, when `line` is a
/// list item, marked with a dash, asterisk, or plus, or a number and a period
/// or parenthesis, then a space; `None` otherwise, or with the cursor before
/// the item's text.
fn list_continuation(line: &str, at: usize) -> Option<ListEnter> {
    let line = line.trim_end_matches(['\n', '\r']);
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let (indent, rest) = line.split_at(indent_len);
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let (marker, next_marker) = if digits > 0 {
        let punct = rest[digits..]
            .chars()
            .next()
            .filter(|c| matches!(c, '.' | ')'))?;
        let number: u64 = rest[..digits].parse().ok()?;
        (&rest[..digits + 1], format!("{}{punct}", number + 1))
    } else {
        let bullet = rest
            .chars()
            .next()
            .filter(|c| matches!(c, '-' | '*' | '+'))?;
        (&rest[..1], bullet.to_string())
    };
    let after = &rest[marker.len()..];
    let space = after.len() - after.trim_start_matches([' ', '\t']).len();
    if space == 0 && !after.is_empty() {
        return None;
    }
    let text_start = indent_len + marker.len() + space;
    if at < text_start.min(line.len()) {
        return None;
    }
    if after.trim().is_empty() {
        return Some(ListEnter::End(indent.to_string()));
    }
    let gap = if space == 0 { " " } else { &after[..space] };
    Some(ListEnter::Next(format!("{indent}{next_marker}{gap}")))
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

    use super::{CloseFile, FileView, language_for};

    /// Common languages are highlighted by their extensions, and some files by
    /// their whole names; every one of them has a grammar in gpui-kit. Files it
    /// has no grammar for are plain text.
    #[test]
    fn files_are_highlighted_in_their_language() {
        use gpui_kit::component::highlighter::LanguageRegistry;

        let cases = [
            ("src/main.rs", "rust"),
            ("app.py", "python"),
            ("index.ts", "typescript"),
            ("view.tsx", "tsx"),
            ("util.mjs", "javascript"),
            ("button.jsx", "javascript"),
            ("main.go", "go"),
            ("lib.h", "c"),
            ("lib.hpp", "cpp"),
            ("Main.java", "java"),
            ("Program.cs", "csharp"),
            ("style.scss", "css"),
            ("page.html", "html"),
            ("README.md", "markdown"),
            ("Cargo.toml", "toml"),
            ("Cargo.lock", "toml"),
            ("config.yml", "yaml"),
            ("data.json", "json"),
            ("run.sh", "bash"),
            (".zshrc", "bash"),
            ("Makefile", "make"),
            ("CMakeLists.txt", "cmake"),
            ("app.rb", "ruby"),
            ("Gemfile", "ruby"),
            ("init.lua", "lua"),
            ("App.swift", "swift"),
            ("main.kt", "kotlin"),
            ("mix.exs", "elixir"),
            ("schema.graphql", "graphql"),
            ("api.proto", "proto"),
            ("change.diff", "diff"),
            ("main.zig", "zig"),
        ];
        let registry = LanguageRegistry::singleton();
        for (file, language) in cases {
            let config = registry
                .language(&language_for(Path::new(file)))
                .unwrap_or_else(|| panic!("{file} has no language"));
            assert!(config.has_grammar(), "{file} has no grammar");
            assert_eq!(
                Some(config.name),
                registry.language(language).map(|config| config.name),
                "{file} isn't highlighted as {language}"
            );
        }
        assert_eq!(
            language_for(Path::new("spec/index.pi")),
            crate::piton_syntax::LANGUAGE_NAME
        );
        for plain in ["LICENSE", "notes.txt"] {
            assert!(
                registry
                    .language(&language_for(Path::new(plain)))
                    .is_none_or(|config| !config.has_grammar()),
                "{plain} is highlighted"
            );
        }
    }
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

    #[test]
    fn backspace_goes_back_to_the_tab_stop() {
        use super::dedent_width;
        assert_eq!(dedent_width("        x", 8), Some(4));
        assert_eq!(dedent_width("      x", 6), Some(2));
        assert_eq!(dedent_width("     x", 5), None);
        assert_eq!(dedent_width("    ", 4), Some(4));
        assert_eq!(dedent_width("  a ", 4), None);
        assert_eq!(dedent_width("x", 0), None);
    }

    #[test]
    fn home_goes_to_the_text_then_the_line() {
        use super::home_column;
        assert_eq!(home_column("    let x = 1;", 10), 4);
        assert_eq!(home_column("    let x = 1;", 4), 0);
        assert_eq!(home_column("    let x = 1;", 0), 4);
        assert_eq!(home_column("    ", 2), 0);
        assert_eq!(home_column("abc", 2), 0);
    }

    #[test]
    fn enter_carries_lists_on() {
        use super::{ListEnter, list_continuation};
        let next = |s: &str| Some(ListEnter::Next(s.into()));
        assert_eq!(list_continuation("- one", 5), next("- "));
        assert_eq!(list_continuation("    * two", 9), next("    * "));
        assert_eq!(list_continuation("  9. nine", 9), next("  10. "));
        assert_eq!(list_continuation("1) a", 4), next("2) "));
        assert_eq!(
            list_continuation("    - ", 6),
            Some(ListEnter::End("    ".into()))
        );
        assert_eq!(list_continuation("-", 1), Some(ListEnter::End("".into())));
        assert_eq!(list_continuation("-x", 2), None);
        assert_eq!(list_continuation("key: value", 5), None);
        assert_eq!(list_continuation("2024 was", 8), None);
        // Before the item's text, <Enter> only breaks the line.
        assert_eq!(list_continuation("- one", 0), None);
    }

    /// <Tab> indents by 4 spaces, and <Backspace> in the indentation takes
    /// back a whole tab stop at a time.
    #[gpui_kit::test]
    async fn tab_stops_are_four_spaces(cx: &mut TestAppContext) {
        let file = temp_file("file-view-tabs.rs", "x");
        init(cx, None);
        let (view, handle) = open(cx, &file, None);
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            view.read(cx).editor.read(cx).value().as_ref() == "x"
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.editor.update(cx, |editor, cx| {
                    editor.set_cursor_position(Position::new(0, 0), window, cx)
                })
            });
        })
        .unwrap();
        let value = |cx: &mut TestAppContext| {
            view.read_with(cx, |v, cx| v.editor.read(cx).value().to_string())
        };
        for _ in 0..2 {
            cx.update_window(handle, |_, window, cx| window.press("tab", cx))
                .unwrap();
            cx.run_until_parked();
        }
        assert_eq!(value(cx), "        x");
        cx.update_window(handle, |_, window, cx| window.press("backspace", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(value(cx), "    x");
        cx.update_window(handle, |_, window, cx| window.press("backspace", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(value(cx), "x");
        std::fs::remove_file(&file).ok();
    }

    /// In the editor itself: <Home> goes to the text, then the line's start,
    /// and <Enter> on a list item starts the next, or ends an empty one.
    #[gpui_kit::test]
    async fn home_and_enter_behave_as_in_a_code_editor(cx: &mut TestAppContext) {
        const TEXT: &str = "    - one";
        let file = temp_file("file-view-keys.md", TEXT);
        init(cx, None);
        let (view, handle) = open(cx, &file, None);
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            view.read(cx).editor.read(cx).value().as_ref() == TEXT
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.editor.update(cx, |editor, cx| {
                    editor.set_cursor_position(Position::new(0, 9), window, cx)
                })
            });
        })
        .unwrap();
        let cursor =
            |cx: &mut TestAppContext| view.read_with(cx, |v, cx| v.editor.read(cx).cursor());
        cx.update_window(handle, |_, window, cx| window.press("home", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(cursor(cx), 4);
        cx.update_window(handle, |_, window, cx| window.press("home", cx))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(cursor(cx), 0);
        cx.update_window(handle, |_, window, cx| window.press("end", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.run_until_parked();
        type_keys(cx, handle, "two");
        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                view.read(cx).editor.read(cx).value().as_ref(),
                "    - one\n    - two\n    "
            );
        });
        std::fs::remove_file(&file).ok();
    }

    /// Typing clears a hover the server showed, straight away.
    #[gpui_kit::test]
    async fn typing_clears_hovers(cx: &mut TestAppContext) {
        const TEXT: &str = "anchor A:\n";
        let file = temp_file("file-view-hover", TEXT);
        init(cx, None);
        let (view, handle) = open(cx, &file, None);
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            view.read(cx).editor.read(cx).value().as_ref() == TEXT
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            let editor = view.read(cx).editor.clone();
            editor.update(cx, |editor, cx| {
                editor.present_hover(
                    0..6,
                    lsp_types::Hover {
                        contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
                            kind: lsp_types::MarkupKind::PlainText,
                            value: "An anchor".into(),
                        }),
                        range: None,
                    },
                    cx,
                );
                editor.focus_handle(cx).focus(window, cx);
            });
            assert!(editor.read(cx).hover_popover().is_some());
        })
        .unwrap();
        type_keys(cx, handle, "x");
        cx.update(|cx| {
            assert!(view.read(cx).editor.read(cx).hover_popover().is_none());
        });
        std::fs::remove_file(&file).ok();
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

    /// The header is as tall as the tab bar, 12 pixels in at its left and
    /// 8 at its right, its buttons 24 pixels tall and centred; the file's
    /// text starts 12 pixels in, under the path, and 8 below the header.
    #[gpui_kit::test]
    async fn the_editor_is_laid_out_plainly(cx: &mut TestAppContext) {
        use gpui_kit::px;
        const TEXT: &str = "first line\nsecond line\n";
        let file = temp_file("file-view-layout", TEXT);
        init(cx, None);
        let (view, handle) = open(cx, &file, None);
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            view.read(cx).editor.read(cx).value().as_ref() == TEXT
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let file = window.find("file-view").bounds();
            let header = window.find("file-header").bounds();
            let save = window.find("save-file").bounds();
            let close = window.find("close-file").bounds();
            let text = view
                .read(cx)
                .editor
                .read(cx)
                .range_to_bounds(&(0..0))
                .expect("no text laid out");
            assert_eq!(header.top(), file.top());
            assert_eq!(header.size.height, px(32.));
            assert_eq!((header.left(), header.right()), (file.left(), file.right()));
            for button in [save, close] {
                assert_eq!(button.size.height, px(24.));
                assert!((button.center().y - header.center().y).abs() <= px(0.5));
            }
            assert_eq!(close.size.width, px(24.));
            assert!((header.right() - close.right() - px(8.)).abs() <= px(0.5));
            assert!((close.left() - save.right() - px(4.)).abs() <= px(0.5));
            // The editor pads its content 10 pixels; set in 2, its line
            // numbers start 12 in, under the path, and the text after them.
            let editor = window.find("file-text").bounds();
            assert_eq!(editor.left() - file.left(), super::TEXT_NUDGE);
            assert_eq!(
                gpui_kit::component::Size::Medium.input_px() + super::TEXT_NUDGE,
                px(12.)
            );
            assert!(
                text.left() > file.left() + px(12.),
                "no line numbers before the text"
            );
            assert!(
                (text.top() - header.bottom() - px(8.)).abs() <= px(1.),
                "the text starts {:?} below the header",
                text.top() - header.bottom()
            );
        })
        .unwrap();
    }

    /// Dragging across text in the editor shows a popover by it: Copy puts
    /// the text on the clipboard, Cut takes it out of the file too, Paste
    /// puts the clipboard in its place, and Send to Prompt hands it on to be
    /// attached. Each closes the popover.
    #[gpui_kit::test]
    async fn selected_text_can_be_cut_copied_pasted_or_sent(cx: &mut TestAppContext) {
        const TEXT: &str = "alpha beta gamma delta\nsecond line\n";
        let file = temp_file("file-view-selection", TEXT);
        init(cx, None);
        let (view, handle) = open(cx, &file, None);
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            view.read(cx).editor.read(cx).value().as_ref() == TEXT
        })
        .await;
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let _subscription = cx.update(|cx| {
            let sent = sent.clone();
            cx.subscribe(
                &view,
                move |_, super::SendToPrompt(text): &super::SendToPrompt, _| {
                    sent.borrow_mut().push(text.clone())
                },
            )
        });

        // Drags across the first line, and says whether the popover showed.
        let select_first_line = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let editor = window.find("file-view").bounds();
                let header = window.find("save-file").bounds();
                let y = header.bottom() + gpui_kit::px(14.);
                window.drag(
                    gpui_kit::point(editor.left() + gpui_kit::px(60.), y),
                    gpui_kit::point(editor.right() - gpui_kit::px(20.), y),
                    cx,
                );
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("editor-selection-popover").is_some()
            })
            .unwrap()
        };
        let selected = |cx: &mut TestAppContext| {
            view.read_with(cx, |view, cx| {
                view.editor.read(cx).selected_text().to_string()
            })
        };
        let click = |cx: &mut TestAppContext, id: &'static str| {
            cx.update_window(handle, |_, window, cx| window.click(id, cx))
                .unwrap();
            cx.run_until_parked();
            assert!(
                view.read_with(cx, |view, _| !view.selection_popover_shown()),
                "{id}"
            );
        };

        assert!(select_first_line(cx), "no popover for the selection");
        let first = selected(cx);
        assert!(
            first.starts_with("lpha") || first.starts_with("alpha"),
            "{first:?}"
        );
        click(cx, "editor-selection-copy");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(first.clone())
        );

        assert!(select_first_line(cx));
        click(cx, "editor-selection-send");
        assert_eq!(*sent.borrow(), [first.clone()]);

        assert!(select_first_line(cx));
        click(cx, "editor-selection-cut");
        let value = view.read_with(cx, |view, cx| view.editor.read(cx).value().to_string());
        assert!(!value.contains(&first), "{value:?}");
        assert!(view.read_with(cx, |view, _| view.is_dirty()));

        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("PASTED".into()));
        cx.update_window(handle, |_, window, cx| {
            let editor = view.read(cx).editor.clone();
            editor.update(cx, |editor, cx| editor.set_value(TEXT, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert!(select_first_line(cx));
        click(cx, "editor-selection-paste");
        let value = view.read_with(cx, |view, cx| view.editor.read(cx).value().to_string());
        assert_eq!(value, TEXT.replacen(&first, "PASTED", 1));

        // Pressing elsewhere closes it, doing nothing.
        assert!(select_first_line(cx));
        cx.update_window(handle, |_, window, cx| window.click("save-file", cx))
            .unwrap();
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| !view.selection_popover_shown()));

        std::fs::remove_file(&file).ok();
    }

    /// Saving a Piton file formats it with `piton format`, on disk and in
    /// the editor.
    #[gpui_kit::test]
    async fn saving_a_piton_file_formats_it(cx: &mut TestAppContext) {
        if crate::piton_build::piton_missing() {
            return;
        }
        let file = std::env::temp_dir().join(format!(
            "suspense-file-view-format-{}.pi",
            std::process::id()
        ));
        std::fs::write(&file, "anchor A:\n  x: 1\n").unwrap();
        init(cx, None);
        let (view, handle) = open(cx, &file, None);
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            view.read(cx).editor.read(cx).value().as_ref() == "anchor A:\n  x: 1\n"
        })
        .await;

        cx.update_window(handle, |_, window, cx| {
            let focus = view.read(cx).editor.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        })
        .unwrap();
        type_keys(cx, handle, "//note\n");
        cx.update_window(handle, |_, window, cx| window.press(SAVE, cx))
            .unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| {
            let view = view.read(cx);
            !view.is_dirty() && view.editor.read(cx).value().starts_with("// note\n")
        })
        .await;
        let saved = std::fs::read_to_string(&file).unwrap();
        assert_eq!(saved, "// note\nanchor A:\n    x: 1\n");
        assert_eq!(
            view.read_with(cx, |view, cx| view.editor.read(cx).value().to_string()),
            saved
        );
        // The cursor stays where it was: just before "anchor".
        let cursor = view.read_with(cx, |view, cx| view.editor.read(cx).selected_range());
        assert_eq!(cursor, 8..8);

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
        if crate::piton_build::piton_missing() {
            return;
        }
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
        if crate::piton_build::piton_missing() {
            return;
        }
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

    /// Formatting on save moves the cursor with the text around it.
    #[test]
    fn the_cursor_keeps_its_place_through_formatting() {
        use super::offset_after_formatting as at;
        let before = "anchor A:\n  name:   Hi\n  kind: x\n";
        let after = "anchor A:\n    name: Hi\n    kind: x\n";
        // Just after "Hi".
        let hi = before.find("Hi").unwrap() + 2;
        assert_eq!(&after[..at(before, after, hi)], "anchor A:\n    name: Hi");
        // At the start of "kind", behind its indentation.
        let kind = before.find("kind").unwrap();
        assert_eq!(&after[at(before, after, kind)..], "kind: x\n");
        // At the start of the second line.
        let line = before.find('\n').unwrap() + 1;
        assert_eq!(at(before, after, line), after.find('\n').unwrap() + 1);
        assert_eq!(at(before, after, before.len()), after.len());
    }

    #[test]
    fn the_cursor_keeps_its_place_through_edits_elsewhere() {
        use super::offset_after_edit;
        let before = "one\ntwo\nthree\n";
        // A line added above: beside the same characters, a line lower.
        let after = "zero\none\ntwo\nthree\n";
        let three = before.find("three").unwrap() + 2;
        assert_eq!(
            offset_after_edit(before, after, three),
            after.find("three").unwrap() + 2
        );
        // Its own line changed: as far along the line that took its place.
        let after = "one\nTWO!\nthree\n";
        assert_eq!(offset_after_edit(before, after, 6), 6);
        // Its line removed: where the lines after it now start.
        let after = "one\nthree\n";
        assert_eq!(offset_after_edit(before, after, 6), 4);
        assert_eq!(offset_after_edit(before, after, before.len()), after.len());
    }

    /// Like `wait_for`, but lets real time pass: the watcher reports from its
    /// own thread, which the test clock does not drive.
    async fn wait_for_disk(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        mut predicate: impl FnMut(&gpui_kit::App) -> bool,
    ) {
        for _ in 0..300 {
            cx.run_until_parked();
            if cx.update(|cx| predicate(cx)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
            cx.executor().advance_clock(Duration::from_millis(50));
        }
        cx.wait_for(handle, Duration::ZERO, |_, cx| predicate(cx))
            .await;
    }

    /// Edited on disk, the file reloads, the cursor beside the same
    /// characters; renamed there, the editor follows it. With unsaved
    /// changes, an edit on disk is asked about instead: kept, reloaded, or
    /// merged, the merge putting its text in the editor. Its own saves are
    /// never taken for changes on disk.
    #[gpui_kit::test]
    async fn follows_its_file_on_disk(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-disk-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        init(cx, None);
        let (view, handle) = open(cx, &file, None);
        let text = |cx: &gpui_kit::App| view.read(cx).editor.read(cx).value().to_string();
        cx.wait_for(handle, TIMEOUT, |_, cx| text(cx) == "one\ntwo\nthree\n")
            .await;
        // Lets the watcher settle on the folder before anything changes.
        std::thread::sleep(Duration::from_millis(200));

        // Edited on disk with nothing unsaved: reloaded at once.
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.editor.update(cx, |editor, cx| {
                    let _ = window;
                    editor.set_selected_range(10..10, cx)
                })
            })
        })
        .unwrap();
        std::fs::write(&file, "zero\none\ntwo\nthree\n").unwrap();
        wait_for_disk(cx, handle, |cx| text(cx) == "zero\none\ntwo\nthree\n").await;
        cx.update(|cx| {
            assert!(!view.read(cx).is_dirty());
            assert_eq!(view.read(cx).editor.read(cx).selected_range(), 15..15);
        });

        // Renamed on disk: followed, in the same editor.
        let renamed = dir.join("b.txt");
        std::fs::rename(&file, &renamed).unwrap();
        wait_for_disk(cx, handle, |cx| view.read(cx).path() == renamed).await;
        assert!(view.read_with(cx, |view, _| view.title().ends_with("b.txt")));

        // Unsaved changes, then an edit on disk: asked about, not reloaded.
        cx.update_window(handle, |_, window, cx| {
            let focus = view.read(cx).editor.read(cx).focus_handle(cx);
            focus.focus(window, cx);
            view.update(cx, |view, cx| {
                view.editor
                    .update(cx, |editor, cx| editor.set_selected_range(0..0, cx))
            });
        })
        .unwrap();
        type_keys(cx, handle, "Hi ");
        assert!(view.read_with(cx, |view, _| view.is_dirty()));
        std::fs::write(&renamed, "zero\none\ntwo\nthree\nfour\n").unwrap();
        wait_for_disk(cx, handle, |cx| view.read(cx).has_conflict()).await;
        assert_eq!(cx.update(|cx| text(cx)), "Hi zero\none\ntwo\nthree\n");

        // Kept: still unsaved, against the file as it now is.
        view.update(cx, |view, cx| view.keep_mine(cx));
        assert!(view.read_with(cx, |view, _| view.is_dirty() && !view.has_conflict()));

        // Changed again, it asks again; reloaded, the changes go.
        std::fs::write(&renamed, "five\n").unwrap();
        wait_for_disk(cx, handle, |cx| view.read(cx).has_conflict()).await;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| view.reload_from_disk(window, cx))
        })
        .unwrap();
        cx.update(|cx| {
            assert_eq!(text(cx), "five\n");
            assert!(!view.read(cx).is_dirty());
        });

        // Merged: every change taken from disk, the merge applied.
        view.update(cx, |view, cx| {
            view.editor
                .update(cx, |editor, cx| editor.set_selected_range(0..0, cx))
        });
        type_keys(cx, handle, "six ");
        std::fs::write(&renamed, "seven\n").unwrap();
        wait_for_disk(cx, handle, |cx| view.read(cx).has_conflict()).await;
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| view.start_merge(window, cx))
        })
        .unwrap();
        let merge = view.read_with(cx, |view, _| view.merge_view()).unwrap();
        cx.wait_for(handle, TIMEOUT, |_, cx| merge.read(cx).diff().has_changes())
            .await;
        assert_eq!(
            merge.read_with(cx, |merge, _| merge.merged_text()),
            Some("six five\n".to_string())
        );
        merge.update(cx, |merge, cx| merge.choose_all(true, cx));
        cx.update_window(handle, |_, window, cx| window.click("merge-apply", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(view.read(cx).merge_view().is_none());
            assert_eq!(text(cx), "seven\n");
            assert!(!view.read(cx).is_dirty());
        });

        // Its own save is not a change on disk.
        view.update(cx, |view, cx| {
            view.editor
                .update(cx, |editor, cx| editor.set_selected_range(0..0, cx))
        });
        type_keys(cx, handle, "eight ");
        cx.update_window(handle, |_, window, cx| window.press(SAVE, cx))
            .unwrap();
        wait_for_disk(cx, handle, |cx| !view.read(cx).is_dirty()).await;
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(10));
            cx.executor().advance_clock(Duration::from_millis(50));
            cx.run_until_parked();
        }
        cx.update(|cx| {
            assert!(!view.read(cx).has_conflict());
            assert_eq!(text(cx), "eight seven\n");
        });
        std::fs::remove_dir_all(&dir).ok();
    }
}
