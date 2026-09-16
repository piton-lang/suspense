//! `piton lsp`: the client for the project's server, the chat input's session
//! over it (completions, and auto-imports for the hidden anchor the input's
//! text is typed into), and the helpers the editor shares with it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use gpui_kit::*;
use lsp_types::{
    CompletionItem, CompletionResponse, CompletionTextEdit, Diagnostic, Position, Range,
};
use serde_json::{Value, json};

use crate::hidden_anchor::{self, HiddenAnchor, Imports};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Most rounds of auto-importing per resolution; an import can bring further
/// names into reach.
const IMPORT_ROUNDS: usize = 3;

type Stdin = Arc<Mutex<ChildStdin>>;
type Pending = Arc<Mutex<HashMap<i64, mpsc::Sender<Result<Value>>>>>;
type Published = Arc<Mutex<PublishedDiagnostics>>;

/// What the server has published, by canonical path, and who to tell when it
/// publishes again.
#[derive(Default)]
struct PublishedDiagnostics {
    by_path: HashMap<PathBuf, Vec<Diagnostic>>,
    listeners: Vec<mpsc::Sender<PathBuf>>,
}

/// A running `piton lsp` process speaking JSON-RPC over stdio.
pub struct LspClient {
    child: Mutex<Child>,
    stdin: Stdin,
    pending: Pending,
    published: Published,
    next_id: AtomicI64,
    project_dir: PathBuf,
}

impl LspClient {
    /// Starts `piton lsp` in `project_dir` and completes the initialize
    /// handshake. Blocks, so call it off the main thread.
    pub fn start(project_dir: &Path) -> Result<Self> {
        let mut child = Command::new("piton")
            .arg("lsp")
            .current_dir(project_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("could not start `piton lsp`")?;
        let stdin: Stdin = Arc::new(Mutex::new(
            child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("`piton lsp` has no stdin"))?,
        ));
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("`piton lsp` has no stdout"))?;
        let pending = Pending::default();
        let published = Published::default();

        std::thread::spawn({
            let stdin = stdin.clone();
            let pending = pending.clone();
            let published = published.clone();
            move || read_loop(BufReader::new(stdout), &stdin, &pending, &published)
        });

        let client = Self {
            child: Mutex::new(child),
            stdin,
            pending,
            published,
            next_id: AtomicI64::new(1),
            project_dir: project_dir.to_path_buf(),
        };
        client.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": file_uri(project_dir),
                "capabilities": {},
            }),
        )?;
        client.notify("initialized", json!({}))?;
        Ok(client)
    }

    /// The project directory the server was started in.
    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    /// Sends a request and blocks until its response arrives.
    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);

        let sent = write_message(
            &self.stdin,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        );
        let response = sent.and_then(|()| {
            rx.recv_timeout(REQUEST_TIMEOUT)
                .map_err(|_| anyhow!("`piton lsp` did not answer {method}"))?
        });
        self.pending.lock().unwrap().remove(&id);
        response
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        write_message(
            &self.stdin,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
    }

    /// What the server last published for the file at canonical `path`.
    pub fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        let published = self.published.lock().unwrap();
        published.by_path.get(path).cloned().unwrap_or_default()
    }

    /// The canonical path of each file the server publishes diagnostics for,
    /// as it publishes them, sent from the client's own thread: collect them
    /// on a timer. Disconnects when the server is gone.
    pub fn watch_diagnostics(&self) -> mpsc::Receiver<PathBuf> {
        let (tx, rx) = mpsc::channel();
        self.published.lock().unwrap().listeners.push(tx);
        rx
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            child.kill().ok();
        }
    }
}

fn read_loop(mut reader: impl BufRead, stdin: &Stdin, pending: &Pending, published: &Published) {
    while let Some(message) = read_message(&mut reader) {
        let id = message.get("id").cloned();
        if let Some(method) = message.get("method") {
            // Requests from the server (such as client/registerCapability)
            // must be answered; of its notifications, only diagnostics are
            // of use.
            if let Some(id) = id {
                write_message(
                    stdin,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": null }),
                )
                .ok();
            } else if method == "textDocument/publishDiagnostics" {
                publish_diagnostics(message.get("params").unwrap_or(&Value::Null), published);
            }
        } else if let Some(id) = id.and_then(|id| id.as_i64())
            && let Some(tx) = pending.lock().unwrap().remove(&id)
        {
            let result = match message.get("error") {
                Some(error) => Err(anyhow!("`piton lsp` error: {error}")),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            tx.send(result).ok();
        }
    }
    // The server is gone: dropping the senders fails every waiting request,
    // and ends every watch on its diagnostics.
    pending.lock().unwrap().clear();
    published.lock().unwrap().listeners.clear();
}

fn publish_diagnostics(params: &Value, published: &Published) {
    let Some(path) = params.get("uri").and_then(Value::as_str).and_then(uri_path) else {
        return;
    };
    let path = canonical(&path);
    let diagnostics = params
        .get("diagnostics")
        .cloned()
        .and_then(|diagnostics| serde_json::from_value(diagnostics).ok())
        .unwrap_or_default();

    let mut published = published.lock().unwrap();
    published.by_path.insert(path.clone(), diagnostics);
    published
        .listeners
        .retain(|listener| listener.send(path.clone()).is_ok());
}

fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn write_message(stdin: &Stdin, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    let mut stdin = stdin.lock().unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
    stdin.write_all(&body)?;
    stdin.flush()?;
    Ok(())
}

/// A `file://` URI for an absolute path, on every platform.
pub fn file_uri(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !path.starts_with('/') {
        uri.push('/');
    }
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                uri.push(byte as char)
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// The path of a `file://` URI, or `None` for any other URI.
pub fn uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Past the authority, which is empty for local files.
    let encoded = &rest[rest.find('/')?..];
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut iter = encoded.bytes();
    while let Some(byte) = iter.next() {
        if byte == b'%' {
            let hex = [iter.next()?, iter.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    let mut path = String::from_utf8(bytes).ok()?;
    // `/C:/…` on Windows.
    if path.as_bytes().get(2) == Some(&b':') {
        path.remove(0);
    }
    Some(PathBuf::from(path))
}

/// `path` with symlinks and `..` resolved where it exists, so paths from the
/// server and from the project tree compare equal.
pub fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// A `piton lsp` session for the chat input. The input's text is presented
/// to the server as the `userPrompt` of a hidden draft anchor, whose imports
/// come from the completions the user accepts and from the server's import
/// quick fixes for names typed by hand.
pub struct PitonSession {
    client: Arc<LspClient>,
    uri: String,
    draft: Mutex<Draft>,
    completion_imports: Mutex<CompletionImports>,
}

struct Draft {
    anchor: HiddenAnchor,
    version: i32,
    opened: bool,
}

/// The import edits `piton lsp` attaches to completions, which the editor
/// itself does not apply.
#[derive(Default)]
struct CompletionImports {
    /// Completions from the last request that carry an import.
    offered: Vec<OfferedCompletion>,
    /// Imports of completions accepted since the last prompt was sent.
    accepted: Imports,
}

struct OfferedCompletion {
    /// Byte offset in the prompt where the completion inserts `new_text`.
    start: usize,
    new_text: String,
    /// The completion's `additionalTextEdits`, as Piton source.
    imports: String,
}

impl PitonSession {
    /// A session over the project's running server.
    pub fn new(client: Arc<LspClient>) -> Self {
        Self {
            uri: file_uri(&hidden_anchor::draft_path(client.project_dir())),
            client,
            draft: Mutex::new(Draft {
                anchor: HiddenAnchor::random(),
                version: 0,
                opened: false,
            }),
            completion_imports: Mutex::default(),
        }
    }

    /// Starts `piton lsp` for `project_dir`, for a session of its own.
    /// Blocking.
    #[cfg(test)]
    pub fn connect(project_dir: &Path) -> Result<Self> {
        Ok(Self::new(Arc::new(LspClient::start(project_dir)?)))
    }

    /// The server the session speaks to.
    pub fn client(&self) -> &Arc<LspClient> {
        &self.client
    }

    /// Completions at byte `offset` of `prompt`, with ranges in the prompt's
    /// own coordinates. Blocking.
    pub fn complete(&self, prompt: &str, offset: usize) -> Result<CompletionResponse> {
        let mut draft = self.draft.lock().unwrap();
        // Import what the prompt already uses first, so members of those
        // anchors complete too.
        self.auto_import(&mut draft, prompt)?;

        let first_line = draft.anchor.prompt_first_line();
        let draft_name = draft.anchor.name().to_string();
        let indent = hidden_anchor::PROMPT_INDENT.len() as u32;
        let cursor = lsp_position(prompt, offset);
        let result = self.client.request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": self.uri },
                "position": {
                    "line": cursor.line + first_line,
                    "character": cursor.character + indent,
                },
            }),
        )?;
        drop(draft);

        let mut offered = Vec::new();
        let mut matches = Vec::new();
        for mut item in completion_items(result)? {
            // The hidden draft anchor is an implementation detail, never a
            // suggestion.
            if item.label == draft_name {
                continue;
            }
            // Import edits target the hidden anchor, not the input: keep them
            // aside until the completion is accepted (see `note_edit`).
            let imports: String = item
                .additional_text_edits
                .take()
                .into_iter()
                .flatten()
                .map(|edit| edit.new_text)
                .collect();

            // Ranges come back in hidden-anchor coordinates; move them into
            // the prompt's, dropping any edit that reaches outside it.
            let keep = match &mut item.text_edit {
                Some(CompletionTextEdit::Edit(edit)) => {
                    match to_input_range(edit.range, first_line, indent) {
                        Some(range) => {
                            edit.range = range;
                            true
                        }
                        None => false,
                    }
                }
                Some(CompletionTextEdit::InsertAndReplace(_)) => false,
                None => true,
            };
            if !keep {
                item.text_edit = None;
            }

            let Some(rank) = match_rank(&item, prompt, offset) else {
                continue;
            };

            if let Some(CompletionTextEdit::Edit(edit)) = &item.text_edit
                && !imports.is_empty()
            {
                offered.push(OfferedCompletion {
                    start: byte_offset(prompt, edit.range.start),
                    new_text: edit.new_text.clone(),
                    imports,
                });
            }
            matches.push((rank, item));
        }

        self.completion_imports.lock().unwrap().offered = offered;
        Ok(CompletionResponse::Array(sort_by_rank(matches)))
    }

    /// Notices an offered completion now in `prompt` where it was offered,
    /// and takes on its import. Cheap enough to call on every edit.
    pub fn note_edit(&self, prompt: &str) {
        let mut completion_imports = self.completion_imports.lock().unwrap();
        let CompletionImports { offered, accepted } = &mut *completion_imports;
        let inserted = offered
            .iter()
            .find(|completion| is_inserted_at(prompt, completion.start, &completion.new_text));
        if let Some(completion) = inserted {
            accepted.add_from_source(&completion.imports);
            offered.clear();
        }
    }

    /// The imports taken on from accepted completions, for tests.
    #[cfg(test)]
    pub fn accepted_imports(&self) -> String {
        format!("{:?}", self.completion_imports.lock().unwrap().accepted)
    }

    /// A freshly named anchor importing what `prompt` uses, ready to send:
    /// the imports of accepted completions, plus quick fixes for the rest.
    /// Blocking.
    pub fn anchor_for(&self, prompt: &str) -> Result<HiddenAnchor> {
        self.note_edit(prompt);
        let accepted = std::mem::take(&mut *self.completion_imports.lock().unwrap()).accepted;

        let mut draft = self.draft.lock().unwrap();
        draft.anchor.imports = accepted;
        self.auto_import(&mut draft, prompt)?;

        let mut anchor = HiddenAnchor::random();
        anchor.imports = std::mem::take(&mut draft.anchor.imports);
        Ok(anchor)
    }

    /// Applies the server's import quick fixes to the draft until nothing the
    /// prompt uses is left unimported.
    fn auto_import(&self, draft: &mut Draft, prompt: &str) -> Result<()> {
        for _ in 0..IMPORT_ROUNDS {
            self.sync(draft, prompt)?;
            let end_line = draft.anchor.prompt_first_line() + prompt.split('\n').count() as u32;
            let actions = self.client.request(
                "textDocument/codeAction",
                json!({
                    "textDocument": { "uri": self.uri },
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": end_line, "character": 0 },
                    },
                    "context": { "diagnostics": [] },
                }),
            )?;

            let import_edits = actions
                .as_array()
                .into_iter()
                .flatten()
                .filter(|action| action.get("kind").and_then(Value::as_str) == Some("quickfix"))
                .filter_map(|action| action.pointer("/edit/changes")?.as_object())
                .flat_map(|changes| changes.values())
                .filter_map(Value::as_array)
                .flatten()
                .filter_map(|edit| edit.get("newText")?.as_str());

            let mut added = false;
            for text in import_edits {
                added |= draft.anchor.imports.add_from_source(text);
            }
            if !added {
                return Ok(());
            }
        }
        // Leave the server's copy matching the final imports.
        self.sync(draft, prompt)
    }

    /// Sends the draft's current source, with accepted completions imported,
    /// to the server.
    fn sync(&self, draft: &mut Draft, prompt: &str) -> Result<()> {
        draft
            .anchor
            .imports
            .extend(&self.completion_imports.lock().unwrap().accepted);
        draft.version += 1;
        let text = draft.anchor.draft_source(prompt);
        if draft.opened {
            self.client.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": self.uri, "version": draft.version },
                    "contentChanges": [{ "text": text }],
                }),
            )
        } else {
            draft.opened = true;
            self.client.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": self.uri,
                        "languageId": "piton",
                        "version": draft.version,
                        "text": text,
                    },
                }),
            )
        }
    }
}

/// The items of a `textDocument/completion` result.
pub fn completion_items(result: Value) -> Result<Vec<CompletionItem>> {
    Ok(match result {
        Value::Null => Vec::new(),
        result => match serde_json::from_value(result)? {
            CompletionResponse::Array(items) => items,
            CompletionResponse::List(list) => list.items,
        },
    })
}

/// `piton lsp` answers with every name in scope, and the editor shows the
/// list as given with its first item selected: narrows `items` to what has
/// been typed of the word being completed at byte `offset` of `text`, best
/// first.
pub fn narrow_completions(
    items: Vec<CompletionItem>,
    text: &str,
    offset: usize,
) -> Vec<CompletionItem> {
    sort_by_rank(
        items
            .into_iter()
            .filter_map(|item| Some((match_rank(&item, text, offset)?, item)))
            .collect(),
    )
}

/// How `item` matches what has been typed of the word it completes: 0 when
/// its name starts with it, 1 when its name contains it, `None` otherwise.
fn match_rank(item: &CompletionItem, text: &str, offset: usize) -> Option<u8> {
    let typed = match &item.text_edit {
        Some(CompletionTextEdit::Edit(edit)) => text
            .get(byte_offset(text, edit.range.start)..byte_offset(text, edit.range.end))
            .unwrap_or_default(),
        _ => {
            let before = text.get(..offset).unwrap_or(text);
            let word_len: usize = before
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
                .map(char::len_utf8)
                .sum();
            &before[before.len() - word_len..]
        }
    }
    .to_lowercase();
    let candidate = item
        .filter_text
        .as_deref()
        .unwrap_or(&item.label)
        .to_lowercase();
    if candidate.starts_with(&typed) {
        Some(0)
    } else if candidate.contains(&typed) {
        Some(1)
    } else {
        None
    }
}

/// Best matches first, each rank in the server's sort order.
fn sort_by_rank(mut matches: Vec<(u8, CompletionItem)>) -> Vec<CompletionItem> {
    matches.sort_by(|(a_rank, a), (b_rank, b)| {
        let sort_key =
            |item: &CompletionItem| item.sort_text.clone().unwrap_or_else(|| item.label.clone());
        a_rank
            .cmp(b_rank)
            .then_with(|| sort_key(a).cmp(&sort_key(b)))
    });
    matches.into_iter().map(|(_, item)| item).collect()
}

/// Whether `text` holds `new_text` at byte `start`, as a whole word: how an
/// accepted completion is noticed.
pub fn is_inserted_at(text: &str, start: usize, new_text: &str) -> bool {
    text.get(start..)
        .and_then(|rest| rest.strip_prefix(new_text))
        .is_some_and(|after| {
            !after.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '-')
        })
}

/// The LSP position (UTF-16 columns) of a byte offset.
pub fn lsp_position(text: &str, offset: usize) -> Position {
    let before = text.get(..offset).unwrap_or(text);
    let line_start = before.rfind('\n').map_or(0, |ix| ix + 1);
    Position::new(
        before.matches('\n').count() as u32,
        before[line_start..].encode_utf16().count() as u32,
    )
}

/// The byte offset of an LSP position (UTF-16 columns), clamped to the text.
pub fn byte_offset(text: &str, position: Position) -> usize {
    let line_start: usize = text
        .split_inclusive('\n')
        .take(position.line as usize)
        .map(str::len)
        .sum();
    let mut utf16 = 0;
    for (ix, c) in text[line_start.min(text.len())..].char_indices() {
        if utf16 >= position.character as usize || c == '\n' {
            return line_start + ix;
        }
        utf16 += c.len_utf16();
    }
    text.len()
}

fn to_input_range(range: Range, first_line: u32, indent: u32) -> Option<Range> {
    let shift = |position: Position| {
        (position.line >= first_line && position.character >= indent)
            .then(|| Position::new(position.line - first_line, position.character - indent))
    };
    Some(Range::new(shift(range.start)?, shift(range.end)?))
}

#[cfg(test)]
mod tests {
    // Explicit imports: globbing `super::*` would bring in GPUI's `test` macro
    // and shadow Rust's `#[test]`.
    use std::path::Path;

    use lsp_types::{Position, Range};
    use serde_json::{Value, json};

    use super::{PitonSession, byte_offset, file_uri, lsp_position, to_input_range, uri_path};

    #[test]
    fn positions_count_utf16_columns() {
        let text = "héllo\nwörld 🙂x";
        let offset = text.find('x').unwrap();
        assert_eq!(lsp_position(text, offset), Position::new(1, 8));
        assert_eq!(byte_offset(text, Position::new(1, 8)), offset);
    }

    #[test]
    fn moves_ranges_into_the_input() {
        let range = Range::new(Position::new(7, 12), Position::new(7, 15));
        assert_eq!(
            to_input_range(range, 5, 8),
            Some(Range::new(Position::new(2, 4), Position::new(2, 7)))
        );
        assert_eq!(to_input_range(range, 8, 8), None);
    }

    #[test]
    fn builds_file_uris() {
        assert_eq!(
            file_uri(Path::new("/home/me/my project/a.pi")),
            "file:///home/me/my%20project/a.pi"
        );
    }

    #[test]
    fn reads_paths_back_from_file_uris() {
        let path = Path::new("/home/me/my project/ünï (1).pi");
        assert_eq!(uri_path(&file_uri(path)).as_deref(), Some(path));
        assert_eq!(
            uri_path("file://localhost/a/b.pi").as_deref(),
            Some(Path::new("/a/b.pi"))
        );
        assert_eq!(uri_path("https://piton-lang.org/spec.md"), None);
    }

    fn session() -> PitonSession {
        PitonSession::connect(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// Asks the real `piton lsp` for completions over this repository's spec.
    #[test]
    fn completes_spec_names() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let session = session();
        let prompt = "Update @{Appl";
        let response =
            serde_json::to_value(session.complete(prompt, prompt.len()).unwrap()).unwrap();

        let items = response
            .as_array()
            .or_else(|| response.pointer("/items").and_then(Value::as_array))
            .unwrap_or_else(|| panic!("{response}"));
        let item = items
            .iter()
            .find(|item| item["label"] == "ApplicationScope")
            .unwrap_or_else(|| panic!("{response}"));
        assert_eq!(
            item["textEdit"]["range"],
            json!({ "start": { "line": 0, "character": 9 }, "end": { "line": 0, "character": 13 } })
        );
        assert!(item.get("additionalTextEdits").is_none_or(Value::is_null));
    }

    /// Accepting a completion takes on the import `piton lsp` attached to it.
    #[test]
    fn accepted_completion_imports_its_name() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let session = session();
        let prompt = "Update @{Appl";
        session.complete(prompt, prompt.len()).unwrap();

        session.note_edit("Update @{App");
        assert!(
            format!("{:?}", session.completion_imports.lock().unwrap().accepted).contains("{}")
        );

        session.note_edit("Update @{ApplicationScope}");
        let accepted = format!("{:?}", session.completion_imports.lock().unwrap().accepted);
        assert!(
            accepted.contains("\"/scope/application\": {\"ApplicationScope\"}"),
            "{accepted}"
        );
    }

    /// Names used in pasted text full of what Piton would read as syntax are
    /// imported all the same: after an unclosed quote or brace, a code fence,
    /// deeper indentation, or what would be a comment.
    #[test]
    fn auto_imports_names_in_pasted_text() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let session = session();
        for pasted in [
            "a \"b @{ApplicationScope}",
            "```\n@{ApplicationScope}",
            "    deep indent\n@{ApplicationScope}",
            "see // @{ApplicationScope}",
            "Fix { \"a\": [1, 2 } @{ApplicationScope}",
            "key: value ${HOME}\n  - @{ApplicationScope}",
        ] {
            let anchor = session.anchor_for(pasted).unwrap();
            let source = anchor.source("");
            assert!(
                source.contains("from /scope/application import ApplicationScope\n"),
                "{pasted:?}:\n{source}"
            );
        }
    }

    /// Lets the real `piton lsp` import the names a prompt uses.
    #[test]
    fn auto_imports_names_used_in_prompt() {
        if crate::piton_build::piton_missing() {
            return;
        }
        let session = session();
        let anchor = session
            .anchor_for("See @{ApplicationScope} and ${MainWindowScope.concept.pitch}")
            .unwrap();
        let source = anchor.source("");
        assert!(
            source.contains("from /scope/application import ApplicationScope\n"),
            "{source}"
        );
        assert!(
            source.contains("from /scope/application/MainWindow import MainWindowScope\n"),
            "{source}"
        );
    }
}
