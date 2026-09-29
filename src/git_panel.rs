//! The git panel at the bottom of the sidebar: the branch and how far it is
//! ahead of or behind its upstream, a summary of what has changed (counts
//! only; the project tree shows which files), a commit message, and buttons to
//! commit everything, push, and pull. A message can be written by the harness
//! from what has changed, quickly, with a small model and no tools.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{
    Button, ButtonCustomVariant, ButtonRounded, ButtonVariants as _,
};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::commit_notes::{self, Note, NotesVersion};
use crate::growing_input::GrowToFit;
use crate::project_directory::ProjectDirectory;
use crate::theme::Hue;

/// How often the summary is read again.
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// The panel's header is on a colour of its own, so that change of colour is
/// the edge above it, with no line between it and the panel before.
pub const COLOUR_IS_EDGE: bool = true;

/// How tall the header, the branch's row, is.
pub const HEADER_HEIGHT: Pixels = px(32.);

/// How tall the panel starts, until the edge above it is dragged: room for
/// the summary, the message, and a few commit notes.
pub const START_HEIGHT: Pixels = px(200.);

/// The shortest it can be dragged: its header alone, the body scrolling
/// beneath it.
pub const MIN_HEIGHT: Pixels = HEADER_HEIGHT;

/// The body's padding at either side and at the bottom.
pub const PADDING: Pixels = px(8.);

/// Between one thing in the body and the next.
pub const GAP: Pixels = px(4.);

/// How tall the summary's line is.
const SUMMARY_HEIGHT: Pixels = px(22.);

/// How tall Generate and Commit are.
const BUTTON_HEIGHT: Pixels = px(22.);

/// A commit note's lines, in its small text.
const NOTE_LINE_HEIGHT: Pixels = px(16.);

/// The most rows a commit note wraps onto before it scrolls.
const NOTE_MAX_ROWS: usize = 12;

/// The most rows the commit message grows to before it scrolls.
const MESSAGE_MAX_ROWS: usize = 10;

/// The most of the diff handed to the harness for a message, so writing one
/// stays quick however much has changed.
const MAX_DIFF_CHARS: usize = 12_000;

/// The branch, and a count of each kind of change.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub has_upstream: bool,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
    pub renamed: usize,
    pub untracked: usize,
    pub conflicted: usize,
    /// Lines added and removed in tracked files.
    pub insertions: usize,
    pub deletions: usize,
}

impl Summary {
    /// The repository `project_dir` is in, or `None` outside of one.
    pub fn read(project_dir: &Path) -> Option<Self> {
        let status = git(
            project_dir,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--branch",
                "--untracked-files=all",
            ],
        )
        .ok()?;
        let mut summary = Self::parse(&status);
        // Against HEAD when there is one: staged and unstaged together.
        let stat = git(project_dir, &["diff", "HEAD", "--numstat"])
            .or_else(|_| git(project_dir, &["diff", "--cached", "--numstat"]))
            .unwrap_or_default();
        (summary.insertions, summary.deletions) = Self::parse_numstat(&stat);
        Some(summary)
    }

    /// Reads `git status --porcelain=v1 -z --branch` output.
    pub fn parse(output: &[u8]) -> Self {
        let mut summary = Self::default();
        let mut records = output.split(|byte| *byte == 0);
        while let Some(record) = records.next() {
            let text = String::from_utf8_lossy(record);
            if let Some(header) = text.strip_prefix("## ") {
                summary.read_branch(header);
                continue;
            }
            if record.len() < 4 {
                continue;
            }
            let (x, y) = (record[0], record[1]);
            if matches!(x, b'R' | b'C') {
                records.next();
            }
            match (x, y) {
                (b'?', b'?') => summary.untracked += 1,
                (b'D', b'D') | (b'A', b'A') | (b'U', _) | (_, b'U') => summary.conflicted += 1,
                (b'R', _) | (_, b'R') => summary.renamed += 1,
                (b'A', _) => summary.added += 1,
                (b'D', _) | (_, b'D') => summary.deleted += 1,
                _ => summary.modified += 1,
            }
        }
        summary
    }

    /// Reads the branch line: `main...origin/main [ahead 1, behind 2]`,
    /// `No commits yet on main`, or `HEAD (no branch)`.
    fn read_branch(&mut self, header: &str) {
        if header.starts_with("HEAD (no branch)") {
            return;
        }
        let header = header.strip_prefix("No commits yet on ").unwrap_or(header);
        let (names, counts) = match header.split_once(" [") {
            Some((names, counts)) => (names, counts.trim_end_matches(']')),
            None => (header, ""),
        };
        let (branch, upstream) = match names.split_once("...") {
            Some((branch, upstream)) => (branch, Some(upstream)),
            None => (names, None),
        };
        self.branch = Some(branch.to_string());
        self.has_upstream = upstream.is_some();
        for count in counts.split(", ") {
            if let Some(n) = count.strip_prefix("ahead ") {
                self.ahead = n.parse().unwrap_or(0);
            } else if let Some(n) = count.strip_prefix("behind ") {
                self.behind = n.parse().unwrap_or(0);
            }
        }
    }

    /// Sums `git diff --numstat` output; binary files count no lines.
    fn parse_numstat(output: &[u8]) -> (usize, usize) {
        String::from_utf8_lossy(output)
            .lines()
            .filter_map(|line| {
                let mut columns = line.split('\t');
                Some((columns.next()?.parse().ok()?, columns.next()?.parse().ok()?))
            })
            .fold((0, 0), |(added, removed), (a, r): (usize, usize)| {
                (added + a, removed + r)
            })
    }

    /// Whether there is anything to commit.
    pub fn has_changes(&self) -> bool {
        self.added + self.modified + self.deleted + self.renamed + self.untracked + self.conflicted
            > 0
    }

    /// The counts that aren't zero, in words.
    pub fn changes(&self) -> Vec<String> {
        [
            (self.modified, "modified"),
            (self.added, "added"),
            (self.deleted, "deleted"),
            (self.renamed, "renamed"),
            (self.untracked, "untracked"),
            (self.conflicted, "conflicted"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, kind)| format!("{count} {kind}"))
        .collect()
    }
}

/// Runs git in `dir`, returning what it printed, or what went wrong.
fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .context("could not run git")?;
    if !output.status.success() {
        let mut message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if message.is_empty() {
            message = String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
        bail!("{message}");
    }
    Ok(output.stdout)
}

/// Commits every change in the repository, new files included, with
/// `message`. The commit notes' own file is left out, since the notes go into
/// the message.
pub fn commit(dir: &Path, message: &str) -> Result<()> {
    let notes = commit_notes::file(Path::new("."));
    let exclude = format!(":(exclude){}", notes.display());
    let mut add = vec!["add", "--all", "--", ":/"];
    // Where the project already ignores the file, it is left out anyway, and
    // naming an ignored file at all, even to exclude it, makes `git add` fail.
    let ignored = Command::new("git")
        .args(["check-ignore", "--quiet", "--"])
        .arg(&notes)
        .current_dir(dir)
        .status()
        .is_ok_and(|status| status.success());
    if !ignored {
        add.push(&exclude);
    }
    git(dir, &add)?;
    git(dir, &["commit", "--quiet", "--message", message])?;
    Ok(())
}

/// Pushes the branch, setting its upstream if it has none.
pub fn push(dir: &Path, has_upstream: bool) -> Result<()> {
    if has_upstream {
        git(dir, &["push", "--quiet"])?;
    } else {
        git(
            dir,
            &["push", "--quiet", "--set-upstream", "origin", "HEAD"],
        )?;
    }
    Ok(())
}

/// What a pull did.
#[derive(Debug, PartialEq)]
pub enum Pulled {
    /// There was nothing new.
    UpToDate,
    /// The branch moved up to its upstream.
    FastForwarded,
    /// The branch and its upstream had both moved on, and were merged.
    Merged,
}

/// Pulls the branch's upstream, merging it if it can be merged without
/// conflicts. If it would conflict, nothing is changed: the pull is cancelled
/// with the conflicting files named, and the repository is never left
/// mid-merge.
pub fn pull(dir: &Path) -> Result<Pulled> {
    git(dir, &["fetch", "--quiet"])?;
    let behind = |range: &str| -> Result<usize> {
        Ok(
            String::from_utf8_lossy(&git(dir, &["rev-list", "--count", range])?)
                .trim()
                .parse()
                .unwrap_or(0),
        )
    };
    if behind("HEAD..@{upstream}")? == 0 {
        return Ok(Pulled::UpToDate);
    }
    if behind("@{upstream}..HEAD")? == 0 {
        git(dir, &["merge", "--quiet", "--ff-only", "@{upstream}"])?;
        return Ok(Pulled::FastForwarded);
    }

    // Tried out first without touching the working tree or the index, so a
    // merge that would conflict is never started.
    let trial = Command::new("git")
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "HEAD",
            "@{upstream}",
        ])
        .current_dir(dir)
        .output()
        .context("could not run git")?;
    match trial.status.code() {
        Some(0) => {}
        Some(1) => {
            // The tree, then the conflicting files, then a blank line and
            // messages.
            let output = String::from_utf8_lossy(&trial.stdout);
            let files: Vec<&str> = output
                .lines()
                .skip(1)
                .take_while(|line| !line.is_empty())
                .collect();
            bail!(
                "Pulling would conflict in {}, so nothing was changed.",
                if files.is_empty() {
                    "some files".to_string()
                } else {
                    files.join(", ")
                }
            );
        }
        _ => bail!("{}", String::from_utf8_lossy(&trial.stderr).trim()),
    }

    if let Err(err) = git(dir, &["merge", "--quiet", "--no-edit", "@{upstream}"]) {
        // Should the merge stop partway anyway, it is undone.
        if git(dir, &["rev-parse", "--quiet", "--verify", "MERGE_HEAD"]).is_ok() {
            git(dir, &["merge", "--abort"]).ok();
            bail!("The merge couldn't be finished, so it was cancelled: {err:#}");
        }
        return Err(err);
    }
    Ok(Pulled::Merged)
}

/// What the harness is handed to write a commit message: the kinds of change,
/// new files' names, and as much of the diff as fits.
fn describe_changes(dir: &Path) -> Result<String> {
    let status = git(dir, &["status", "--short", "--untracked-files=all"])?;
    let diff = git(dir, &["diff", "HEAD"])
        .or_else(|_| git(dir, &["diff", "--cached"]))
        .unwrap_or_default();
    let mut diff = String::from_utf8_lossy(&diff).into_owned();
    if let Some((end, _)) = diff.char_indices().nth(MAX_DIFF_CHARS) {
        diff.truncate(end);
        diff.push_str("\n[diff cut short]");
    }
    Ok(format!(
        "Files changed (git status --short):\n{}\nDiff:\n{diff}",
        String::from_utf8_lossy(&status)
    ))
}

/// Has the harness write a commit message for what has changed, quickly: a
/// small model, low effort, and no tools, so it answers from the changes it
/// is handed alone.
pub fn generate_message(dir: &Path) -> Result<String> {
    let changes = describe_changes(dir)?;
    // Written in system-prompts/commit-message.pi.
    let prompt = format!(
        "{}\n\n{changes}",
        crate::baked_prompts::commit_message::REQUEST
    );
    let message = crate::harness::ask_quickly(dir, &prompt)?;
    if message.is_empty() {
        bail!("the harness wrote no message");
    }
    Ok(message)
}

/// What the panel is doing, while it does it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Busy {
    Commit,
    Push,
    Pull,
    Generate,
}

/// A commit note, as its row's input.
struct NoteRow {
    id: u64,
    input: Entity<TextareaState>,
    _subscription: Subscription,
}

pub struct GitPanel {
    root: Option<PathBuf>,
    /// `None` outside a git repository, where the panel isn't shown.
    summary: Option<Summary>,
    message: Entity<EditorState>,
    /// How the message grows to fit its text.
    message_fit: GrowToFit,
    /// The commit notes, as rows that can be edited, in order.
    notes: Vec<NoteRow>,
    busy: Option<Busy>,
    _refresh: Task<()>,
    /// The commit message each project switched away from was left with,
    /// for when it's back.
    left_messages: HashMap<PathBuf, String>,
    /// Its body, beneath the header, scrolls when the panel is shorter than
    /// it.
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl GitPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .soft_wrap(true)
                // No empty rows below the last line: the box is sized to its
                // text.
                .scroll_beyond_last_line(Some(0))
                .placeholder("Commit Message…")
        });
        let subscriptions = vec![
            cx.observe_global_in::<ProjectDirectory>(window, |this, window, cx| {
                this.open(window, cx);
                this.load_notes(window, cx);
            }),
            // A task that finished has written a note.
            cx.observe_global_in::<NotesVersion>(window, |this, window, cx| {
                this.load_notes(window, cx)
            }),
            // The commit button follows whether there is a message, and the
            // box grows to fit it.
            cx.subscribe(&message, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        let mut this = Self {
            root: None,
            summary: None,
            message,
            message_fit: GrowToFit::new(MESSAGE_MAX_ROWS),
            notes: Vec::new(),
            busy: None,
            _refresh: Task::ready(()),
            left_messages: HashMap::new(),
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        this.open(window, cx);
        this.load_notes(window, cx);
        this
    }

    /// Shows the project's commit notes, keeping the rows of those already
    /// shown, so an edit under way isn't disturbed.
    fn load_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let notes = self
            .root
            .as_deref()
            .map(commit_notes::load)
            .unwrap_or_default();
        let mut rows = std::mem::take(&mut self.notes);
        self.notes = notes
            .into_iter()
            .map(|note| match rows.iter().position(|row| row.id == note.id) {
                Some(ix) => {
                    let row = rows.remove(ix);
                    let focused = row.input.read(cx).focus_handle(cx).is_focused(window);
                    if !focused && row.input.read(cx).value().as_ref() != note.text {
                        row.input
                            .update(cx, |input, cx| input.set_value(note.text, window, cx));
                    }
                    row
                }
                None => self.note_row(note, window, cx),
            })
            .collect();
        cx.notify();
    }

    fn note_row(&mut self, note: Note, window: &mut Window, cx: &mut Context<Self>) -> NoteRow {
        let id = note.id;
        // Wraps onto as many lines as the note needs.
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, NOTE_MAX_ROWS)
                .default_value(note.text)
        });
        // Saved as it is edited.
        let subscription = cx.subscribe(&input, move |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && let Some(root) = &this.root
            {
                commit_notes::edit(root, id, &input.read(cx).value()).ok();
                cx.notify();
            }
        });
        NoteRow {
            id,
            input,
            _subscription: subscription,
        }
    }

    /// The notes as they read now, edits included.
    fn current_notes(&self, cx: &App) -> Vec<Note> {
        self.notes
            .iter()
            .map(|row| Note {
                id: row.id,
                text: row.input.read(cx).value().to_string(),
            })
            .collect()
    }

    fn remove_note(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(root) = &self.root {
            commit_notes::remove(root, id).ok();
            commit_notes::changed(cx);
        }
    }

    #[cfg(test)]
    pub fn summary(&self) -> Option<&Summary> {
        self.summary.as_ref()
    }

    /// Follows the project that is open, reading its summary now and then.
    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = ProjectDirectory::get(cx);
        if root == self.root {
            return;
        }
        // The project left keeps its message as far as it was written.
        if let Some(left) = self.root.take() {
            let message = self.message.read(cx).value().to_string();
            self.left_messages.insert(left, message);
        }
        let message = root
            .as_ref()
            .and_then(|root| self.left_messages.remove(root))
            .unwrap_or_default();
        if self.message.read(cx).value() != message.as_str() {
            self.message
                .update(cx, |editor, cx| editor.set_value(message, window, cx));
        }
        self.root = root.clone();
        self.summary = None;
        self._refresh = match root {
            Some(root) => cx.spawn(async move |this, cx| {
                loop {
                    let summary = cx
                        .background_spawn({
                            let root = root.clone();
                            async move { Summary::read(&root) }
                        })
                        .await;
                    let updated = this.update(cx, |this, cx| {
                        if this.root.as_ref() == Some(&root) && this.summary != summary {
                            this.summary = summary;
                            cx.notify();
                        }
                    });
                    if updated.is_err() {
                        break;
                    }
                    cx.background_executor().timer(REFRESH_INTERVAL).await;
                }
            }),
            None => Task::ready(()),
        };
        cx.notify();
    }

    /// Reads the summary again straight away.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let summary = cx
                .background_spawn({
                    let root = root.clone();
                    async move { Summary::read(&root) }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.root.as_ref() == Some(&root) {
                    this.summary = summary;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Runs `work` in the background as `busy`, then shows `done` or why it
    /// failed, and reads the summary again.
    fn run<T: Send + 'static>(
        &mut self,
        busy: Busy,
        work: impl FnOnce(&Path) -> Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.root.clone().filter(|_| self.busy.is_none()) else {
            return;
        };
        self.busy = Some(busy);
        cx.notify();
        let task = cx.background_spawn(async move { work(&root) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(value) => done(this, value, window, cx),
                    Err(err) => {
                        let title = match busy {
                            Busy::Commit => "Could not commit",
                            Busy::Push => "Could not push",
                            Busy::Pull => "Could not pull",
                            Busy::Generate => "Could not write a commit message",
                        };
                        window.push_notification(
                            Notification::error(format!("{err:#}")).title(title),
                            cx,
                        );
                    }
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Commits with the message typed and the commit notes beneath it.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message =
            commit_notes::message(&self.message.read(cx).value(), &self.current_notes(cx));
        if message.is_empty() {
            return;
        }
        self.run(
            Busy::Commit,
            move |root| commit(root, &message),
            |this, (), window, cx| {
                this.message
                    .update(cx, |editor, cx| editor.set_value("", window, cx));
                // The notes went into the commit.
                if let Some(root) = &this.root {
                    commit_notes::save(root, &[]).ok();
                }
                commit_notes::changed(cx);
                window.push_notification(Notification::success("Committed"), cx);
            },
            window,
            cx,
        );
    }

    fn push(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_upstream = self
            .summary
            .as_ref()
            .is_some_and(|summary| summary.has_upstream);
        self.run(
            Busy::Push,
            move |root| push(root, has_upstream),
            |_, (), window, cx| window.push_notification(Notification::success("Pushed"), cx),
            window,
            cx,
        );
    }

    fn pull(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run(
            Busy::Pull,
            pull,
            |_, pulled, window, cx| {
                let message = match pulled {
                    Pulled::UpToDate => "Already up to date",
                    Pulled::FastForwarded => "Pulled",
                    Pulled::Merged => "Pulled and merged",
                };
                window.push_notification(Notification::success(message), cx)
            },
            window,
            cx,
        );
    }

    fn generate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run(
            Busy::Generate,
            generate_message,
            |this, message, window, cx| {
                this.message
                    .update(cx, |editor, cx| editor.set_value(message, window, cx));
            },
            window,
            cx,
        );
    }

    #[cfg(test)]
    pub fn message(&self, cx: &App) -> String {
        self.message.read(cx).value().to_string()
    }

    #[cfg(test)]
    pub fn set_message(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.message
            .update(cx, |editor, cx| editor.set_value(text, window, cx));
        cx.notify();
    }
}

impl GitPanel {
    /// Whether it shows, which it does only inside a git repository.
    pub fn is_shown(&self) -> bool {
        self.summary.is_some()
    }
}

/// `input` without the padding a multi-line input keeps around its text,
/// which can't be set, so the box around it pads it as it needs.
fn flush(input: impl IntoElement) -> Div {
    let (x, y) = (
        gpui_kit::component::Size::Medium.input_px(),
        gpui_kit::component::Size::Medium.input_py(),
    );
    div().relative().mx(-x).my(-y).child(input)
}

impl Render for GitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(summary) = self.summary.clone() else {
            return div().id("git-panel").into_any_element();
        };
        let palette = *crate::theme::palette(cx);
        let muted = cx.theme().muted_foreground;
        let busy = self.busy;
        let has_message =
            !commit_notes::message(&self.message.read(cx).value(), &self.current_notes(cx))
                .is_empty();

        // The header: the branch, on the ribbon's tab row colour.
        let branch = h_flex()
            .flex_1()
            .min_w_0()
            .gap_1p5()
            .child(Icon::new(IconName::GitBranch).small().text_color(muted))
            .child(
                div().truncate().child(
                    summary
                        .branch
                        .clone()
                        .unwrap_or_else(|| "Detached HEAD".into()),
                ),
            )
            .when(summary.ahead > 0, |row| {
                row.child(div().text_color(muted).child(format!("↑{}", summary.ahead)))
            })
            .when(summary.behind > 0, |row| {
                row.child(
                    div()
                        .text_color(muted)
                        .child(format!("↓{}", summary.behind)),
                )
            });
        let pull = Button::new("git-pull")
            .ghost()
            .xsmall()
            .rounded(ButtonRounded::None)
            .icon(IconName::CloudDownload)
            .tooltip("Pull, merging if there are no conflicts")
            .loading(busy == Some(Busy::Pull))
            .disabled(busy.is_some() || !summary.has_upstream)
            .on_click(cx.listener(|this, _, window, cx| this.pull(window, cx)));
        let push = Button::new("git-push")
            .ghost()
            .xsmall()
            .rounded(ButtonRounded::None)
            .icon(IconName::CloudUpload)
            .tooltip(if summary.has_upstream {
                "Push"
            } else {
                "Push, setting the branch's upstream on origin"
            })
            .loading(busy == Some(Busy::Push))
            .disabled(busy.is_some() || summary.branch.is_none())
            .on_click(cx.listener(|this, _, window, cx| this.push(window, cx)));
        let header = h_flex()
            .id("git-header")
            .flex_none()
            .h(HEADER_HEIGHT)
            .px(PADDING)
            .gap_1()
            .items_center()
            .bg(crate::theme::color(palette.ribbon_tabs))
            .child(branch)
            .child(pull)
            .child(push);

        // The summary: small and muted, with the lines added and removed at
        // its right.
        let changes = summary.changes();
        let counts = if changes.is_empty() {
            "No changes".to_string()
        } else {
            changes.join(" · ")
        };
        let lines = (summary.insertions + summary.deletions > 0).then(|| {
            h_flex()
                .flex_none()
                .gap_1p5()
                .child(
                    div()
                        .text_color(Hue::Green.of(&palette))
                        .child(format!("+{}", summary.insertions)),
                )
                .child(
                    div()
                        .text_color(Hue::Red.of(&palette))
                        .child(format!("−{}", summary.deletions)),
                )
        });
        let summary_row = h_flex()
            .id("git-summary")
            .h(SUMMARY_HEIGHT)
            .gap_2()
            .items_center()
            .justify_between()
            .text_xs()
            .child(div().min_w_0().truncate().text_color(muted).child(counts))
            .children(lines);
        // Lets UI tests find the summary; inert in normal builds.
        let summary_row = gpui_kit::TestSupportExt::test_support(summary_row);

        // Generate at the left of the row beneath the message, on the tab
        // row's colour, and Commit at its right, as the ribbon's buttons are.
        let tabs = crate::theme::color(palette.ribbon_tabs);
        let generate = Button::new("git-generate-message")
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(tabs)
                    .hover(crate::theme::color(palette.base))
                    .active(crate::theme::color(palette.ribbon))
                    .foreground(cx.theme().foreground),
            )
            // gpui-kit keeps only a fifth of a custom button's resting
            // colour, so the colour is given to the button itself, as the
            // ribbon's commands do.
            .bg(tabs)
            .xsmall()
            .h(BUTTON_HEIGHT)
            .rounded(ButtonRounded::None)
            .icon(IconName::Sparkles)
            .label("Generate")
            .tooltip("Have the harness write a message from what has changed")
            .loading(busy == Some(Busy::Generate))
            .disabled(busy.is_some() || !summary.has_changes())
            .on_click(cx.listener(|this, _, window, cx| this.generate(window, cx)));
        let commit = Button::new("git-commit")
            .custom(crate::ribbon::command_colors(cx))
            .bg(crate::ribbon::command_shades(cx).0)
            .xsmall()
            .h(BUTTON_HEIGHT)
            .rounded(ButtonRounded::None)
            .label("Commit")
            .tooltip("Commit every change, new files included, with the message and notes")
            .loading(busy == Some(Busy::Commit))
            .disabled(busy.is_some() || !has_message || !summary.has_changes())
            .on_click(cx.listener(|this, _, window, cx| this.commit(window, cx)));

        let body = v_flex()
            .id("git-body")
            .flex_none()
            .px(PADDING)
            .pb(PADDING)
            .gap(GAP)
            .bg(crate::theme::color(palette.ribbon))
            .child(summary_row)
            // Each note in a box of its own, wrapping, with a button to remove
            // it at its top right while it is hovered.
            .children(self.notes.iter().map(|row| {
                let id = row.id;
                let group = SharedString::from(format!("commit-note-{id}"));
                let note = div().id(("commit-note", id as usize));
                // Lets UI tests find the note; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(note)
                    .group(group.clone())
                    .relative()
                    .py_1()
                    .pl_1p5()
                    .pr_5()
                    .bg(tabs)
                    .child(flush(
                        Textarea::new(&row.input)
                            .appearance(false)
                            .text_xs()
                            .line_height(NOTE_LINE_HEIGHT),
                    ))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .opacity(0.)
                            .group_hover(group, |style| style.opacity(1.))
                            .child(
                                Button::new(("remove-commit-note", id as usize))
                                    .ghost()
                                    .xsmall()
                                    .rounded(ButtonRounded::None)
                                    .icon(IconName::X)
                                    .tooltip("Remove this note")
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.remove_note(id, cx)),
                                    ),
                            ),
                    )
            }))
            .child({
                let (height, _) = self.message_fit.heights(&self.message, window, cx);
                let message = div()
                    .id("git-commit-message")
                    .relative()
                    .py(px(3.))
                    .px_1p5()
                    .bg(crate::theme::color(palette.darkest))
                    // The tracker over the editor alone, not its padding.
                    .child(
                        flush(Editor::new(&self.message).appearance(false).h(height)).child(
                            GrowToFit::tracker(
                                &self.message,
                                cx.entity().downgrade(),
                                |this: &mut Self| &mut this.message_fit,
                            ),
                        ),
                    );
                // Lets UI tests find the message; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(message)
            })
            .child(
                h_flex()
                    .gap(GAP)
                    .justify_between()
                    .child(generate)
                    .child(commit),
            );

        // No line above it, nor between its header and body: the change of
        // colour is the edge.
        // It fills the height its sidebar gives it, and its body scrolls
        // beneath the header when it is taller than that, on the body's
        // colour all the way down.
        let scrolled = div()
            .id("git-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(gpui_kit::TestSupportExt::test_support(body));
        let panel = v_flex()
            .id("git-panel")
            .size_full()
            .text_sm()
            .bg(crate::theme::color(palette.ribbon))
            .child(gpui_kit::TestSupportExt::test_support(header))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(crate::scrollbar::with_scrollbar(
                        "git-scroll",
                        &self.scroll,
                        // Lets UI tests find it; inert in normal builds.
                        gpui_kit::TestSupportExt::test_support(scrolled),
                        true,
                        None,
                        cx,
                    )),
            );
        // Lets UI tests find the panel; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(panel).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
    use gpui_kit::{AppContext as _, TestAppContext};

    use super::{GitPanel, Summary};
    use crate::piton_syntax;
    use crate::project_directory::ProjectDirectory;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// In a real repository with a remote: the panel counts the changes, and
    /// commits them all with the message written, then pushes the commit.
    #[gpui_kit::test]
    async fn commits_everything_and_pushes(cx: &mut TestAppContext) {
        let base = std::env::temp_dir().join(format!("suspense-git-panel-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let (repo, remote) = (base.join("repo"), base.join("remote.git"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--bare", "-q"]);
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        std::fs::write(repo.join("readme.md"), "hello\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&repo, &["push", "-q", "-u", "origin", "main"]);
        std::fs::write(repo.join("readme.md"), "hello again\n").unwrap();
        std::fs::write(repo.join("new.md"), "new\n").unwrap();

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(repo.clone(), cx);
        });
        let mut panel = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| GitPanel::new(window, cx));
            panel = Some(view.clone());
            Root::new(view, window, cx)
        });
        let panel = panel.unwrap();
        let handle = window.into();
        cx.wait_for(handle, Duration::from_secs(5), |window, _| {
            window.try_find("git-commit").is_some()
        })
        .await;
        panel.read_with(cx, |panel, _| {
            let summary = panel.summary().unwrap();
            assert_eq!(summary.branch.as_deref(), Some("main"));
            assert_eq!((summary.modified, summary.untracked), (1, 1));
            assert_eq!((summary.insertions, summary.deletions), (1, 1));
        });

        // The message starts at a single row, grows a row a line, and stops
        // growing at its most rows.
        let message_height = |text: String, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                panel.update(cx, |panel, cx| panel.set_message(&text, window, cx));
                for _ in 0..3 {
                    window.render_frame(cx);
                }
                let line_height = panel.read(cx).message.read(cx).line_height().unwrap();
                let height = window.find("git-commit-message").bounds().size.height;
                (height, line_height)
            })
            .unwrap()
        };
        let (one_row, line_height) = message_height(String::new(), cx);
        let (two_lines, _) = message_height("Summary\n\n- detail".into(), cx);
        assert!(
            (two_lines - one_row - line_height * 2.).abs() <= gpui_kit::px(1.),
            "{two_lines:?} for three lines, {one_row:?} for one"
        );
        let many: Vec<String> = (0..30).map(|n| format!("line {n}")).collect();
        let (most, _) = message_height(many.join("\n"), cx);
        let rows = super::MESSAGE_MAX_ROWS as f32 - 1.;
        assert!(
            (most - one_row - line_height * rows).abs() <= gpui_kit::px(1.),
            "{most:?} for thirty lines"
        );
        assert!(
            one_row < line_height * 2.5,
            "{one_row:?} is more than a row"
        );

        cx.update_window(handle, |_, window, cx| {
            panel.update(cx, |panel, cx| {
                panel.set_message("Say hello again", window, cx);
                panel.commit(window, cx);
            });
        })
        .unwrap();
        let start = std::time::Instant::now();
        while !git(&repo, &["log", "-1", "--format=%s"]).contains("Say hello again") {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "nothing was committed"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            git(&repo, &["status", "--porcelain"]),
            "",
            "not everything was committed"
        );

        cx.wait_for(handle, Duration::from_secs(5), |_, cx| {
            panel
                .read(cx)
                .summary()
                .is_some_and(|summary| summary.ahead == 1)
                && panel.read(cx).busy.is_none()
        })
        .await;
        cx.update_window(handle, |_, window, cx| window.click("git-push", cx))
            .unwrap();
        let start = std::time::Instant::now();
        while !git(&remote, &["log", "-1", "--format=%s", "main"]).contains("Say hello again") {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "nothing was pushed"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::remove_dir_all(&base).ok();
    }

    /// The branch line and each kind of change are counted; renames are
    /// counted once, with the path they came from skipped.
    #[test]
    fn counts_each_kind_of_change() {
        let output = [
            "## main...origin/main [ahead 2, behind 1]",
            " M src/main.rs",
            "M  src/lib.rs",
            "A  src/new.rs",
            " D src/gone.rs",
            "R  src/renamed.rs",
            "src/old.rs",
            "?? notes.md",
            "?? draft.md",
            "UU src/clash.rs",
        ]
        .join("\0");
        let summary = Summary::parse(output.as_bytes());
        assert_eq!(summary.branch.as_deref(), Some("main"));
        assert!(summary.has_upstream);
        assert_eq!((summary.ahead, summary.behind), (2, 1));
        assert_eq!(summary.modified, 2);
        assert_eq!(summary.added, 1);
        assert_eq!(summary.deleted, 1);
        assert_eq!(summary.renamed, 1);
        assert_eq!(summary.untracked, 2);
        assert_eq!(summary.conflicted, 1);
        assert_eq!(
            summary.changes(),
            [
                "2 modified",
                "1 added",
                "1 deleted",
                "1 renamed",
                "2 untracked",
                "1 conflicted"
            ]
        );
    }

    /// A branch with no upstream, a new repository, and a detached HEAD.
    #[test]
    fn reads_every_branch_line() {
        let local = Summary::parse(b"## feature");
        assert_eq!(local.branch.as_deref(), Some("feature"));
        assert!(!local.has_upstream && !local.has_changes());
        assert!(local.changes().is_empty());

        let new = Summary::parse(b"## No commits yet on main");
        assert_eq!(new.branch.as_deref(), Some("main"));

        let detached = Summary::parse(b"## HEAD (no branch)");
        assert_eq!(detached.branch, None);
    }

    /// Lines added and removed are summed; binary files count none.
    #[test]
    fn sums_lines_changed() {
        let numstat = b"10\t2\tsrc/main.rs\n3\t0\tsrc/lib.rs\n-\t-\timage.png\n";
        assert_eq!(Summary::parse_numstat(numstat), (13, 2));
    }

    /// Writes a real commit message for this repository with the real
    /// harness. Slow and needs the network, so only run by hand.
    #[test]
    #[ignore]
    fn writes_a_message_for_this_repository() {
        let started = std::time::Instant::now();
        let message = super::generate_message(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        println!("{:?}\n{message}", started.elapsed());
        assert!(message.lines().next().unwrap().chars().count() <= 100);
    }

    /// A remote, and two clones of it, `mine` and `theirs`, each able to
    /// commit, in a fresh temporary directory named after `name`.
    fn clones(name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let base =
            std::env::temp_dir().join(format!("suspense-pull-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let (remote, mine, theirs) = (
            base.join("remote.git"),
            base.join("mine"),
            base.join("theirs"),
        );
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        for clone in [&mine, &theirs] {
            git(
                &base,
                &[
                    "clone",
                    "-q",
                    remote.to_str().unwrap(),
                    clone.to_str().unwrap(),
                ],
            );
            git(clone, &["config", "user.name", "Test"]);
            git(clone, &["config", "user.email", "test@example.com"]);
        }
        std::fs::write(mine.join("shared.txt"), "one\ntwo\nthree\n").unwrap();
        git(&mine, &["add", "."]);
        git(&mine, &["commit", "-q", "-m", "first"]);
        git(&mine, &["push", "-q", "-u", "origin", "main"]);
        git(&theirs, &["pull", "-q", "origin", "main"]);
        git(&theirs, &["branch", "-q", "--set-upstream-to=origin/main"]);
        (base, mine, theirs)
    }

    /// Nothing new pulls as up to date, and only new commits fast-forward.
    #[test]
    fn pull_fast_forwards() {
        let (base, mine, theirs) = clones("forward");
        assert_eq!(super::pull(&theirs).unwrap(), super::Pulled::UpToDate);
        std::fs::write(mine.join("new.txt"), "new\n").unwrap();
        git(&mine, &["add", "."]);
        git(&mine, &["commit", "-q", "-m", "new"]);
        git(&mine, &["push", "-q"]);
        assert_eq!(super::pull(&theirs).unwrap(), super::Pulled::FastForwarded);
        assert!(theirs.join("new.txt").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    /// When both sides moved on without touching the same lines, pulling
    /// merges them.
    #[test]
    fn pull_merges_without_conflicts() {
        let (base, mine, theirs) = clones("merge");
        std::fs::write(mine.join("shared.txt"), "ONE\ntwo\nthree\n").unwrap();
        git(&mine, &["commit", "-q", "-am", "mine"]);
        git(&mine, &["push", "-q"]);
        std::fs::write(theirs.join("other.txt"), "theirs\n").unwrap();
        git(&theirs, &["add", "."]);
        git(&theirs, &["commit", "-q", "-m", "theirs"]);

        assert_eq!(super::pull(&theirs).unwrap(), super::Pulled::Merged);
        assert_eq!(
            std::fs::read_to_string(theirs.join("shared.txt")).unwrap(),
            "ONE\ntwo\nthree\n"
        );
        assert!(theirs.join("other.txt").exists());
        assert_eq!(git(&theirs, &["status", "--porcelain"]), "");
        std::fs::remove_dir_all(&base).ok();
    }

    /// When the merge would conflict, the pull is cancelled with nothing
    /// changed: the same commit, the same files, and no merge in progress.
    #[test]
    fn pull_cancels_instead_of_conflicting() {
        let (base, mine, theirs) = clones("conflict");
        std::fs::write(mine.join("shared.txt"), "one\nmine\nthree\n").unwrap();
        git(&mine, &["commit", "-q", "-am", "mine"]);
        git(&mine, &["push", "-q"]);
        std::fs::write(theirs.join("shared.txt"), "one\ntheirs\nthree\n").unwrap();
        git(&theirs, &["commit", "-q", "-am", "theirs"]);
        let head = git(&theirs, &["rev-parse", "HEAD"]);

        let err = super::pull(&theirs).unwrap_err().to_string();
        assert!(err.contains("shared.txt"), "{err}");
        assert_eq!(git(&theirs, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&theirs, &["status", "--porcelain"]), "");
        assert!(
            !theirs.join(".git/MERGE_HEAD").exists(),
            "a merge was left in progress"
        );
        assert_eq!(
            std::fs::read_to_string(theirs.join("shared.txt")).unwrap(),
            "one\ntheirs\nthree\n"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Everything is committed but the notes' own file, whether or not the
    /// project ignores it.
    #[test]
    fn commits_leave_out_the_notes_file_even_when_it_is_ignored() {
        for ignored in [false, true] {
            let dir = std::env::temp_dir().join(format!(
                "suspense-commit-ignored-{ignored}-{}",
                std::process::id()
            ));
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-q", "-b", "main"]);
            git(&dir, &["config", "user.name", "Test"]);
            git(&dir, &["config", "user.email", "test@example.com"]);
            if ignored {
                std::fs::write(dir.join(".gitignore"), "/.suspense/commit-notes.json\n").unwrap();
            }
            std::fs::write(dir.join("readme.md"), "hello\n").unwrap();
            crate::commit_notes::add(&dir, "Add the readme").unwrap();

            super::commit(&dir, "First")
                .unwrap_or_else(|err| panic!("ignored {ignored}: could not commit: {err:#}"));
            let files = git(&dir, &["show", "--name-only", "--format=", "HEAD"]);
            assert!(files.contains("readme.md"), "ignored {ignored}: {files}");
            assert!(
                !files.contains("commit-notes"),
                "ignored {ignored}: {files}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// A repository with a change and two commit notes, for the panel's look.
    fn repository_with_notes(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("suspense-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.name", "Test"]);
        git(&dir, &["config", "user.email", "test@example.com"]);
        std::fs::write(dir.join("readme.md"), "hello\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("readme.md"), "hello again\n").unwrap();
        for note in [
            "Rewrite Editor spec with marquee selection and push/pull",
            "Fix scrolling",
        ] {
            crate::commit_notes::add(&dir, note).unwrap();
        }
        dir
    }

    /// Opens the panel on `dir` in a window 260 pixels wide, as the sidebar
    /// starts, once it shows its notes.
    async fn open_panel(
        dir: &Path,
        cx: &mut TestAppContext,
    ) -> (gpui_kit::Entity<GitPanel>, gpui_kit::AnyWindowHandle) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(dir.to_path_buf(), cx);
        });
        let mut panel = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| GitPanel::new(window, cx));
            panel = Some(view.clone());
            Root::new(view, window, cx)
        });
        let handle: gpui_kit::AnyWindowHandle = window.into();
        gpui_kit::VisualTestContext::from_window(handle, cx)
            .simulate_resize(gpui_kit::size(gpui_kit::px(260.), gpui_kit::px(600.)));
        cx.wait_for(handle, Duration::from_secs(5), |window, _| {
            window.try_find(("commit-note", 1usize)).is_some()
        })
        .await;
        (panel.unwrap(), handle)
    }

    /// Renders the panel's frame again until its layout settles.
    fn settle(handle: gpui_kit::AnyWindowHandle, cx: &mut TestAppContext) {
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .unwrap();
        }
    }

    /// The header is 32 pixels tall on the ribbon's tab row colour, the body
    /// beneath it on the ribbon's command area colour, padded 8 pixels at
    /// either side and at the bottom, with 4 between one thing and the next;
    /// each note in a square box on the tab row's colour, wrapping, and the
    /// message on the darkest surface, in dark and light mode alike.
    #[gpui_kit::test]
    async fn the_header_and_body_take_the_ribbons_colours(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        use gpui_kit::px;
        let dir = repository_with_notes("git-panel-look");
        let (_, handle) = open_panel(&dir, cx).await;
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            cx.update(|cx| Theme::change(mode, None, cx));
            settle(handle, cx);
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                let palette = crate::theme::palette(cx);
                let frame = crate::frame_image::Frame::of(window);
                let bounds = |id: &'static str| window.find(id).bounds();
                let (panel, header, body) = (
                    bounds("git-panel"),
                    bounds("git-header"),
                    bounds("git-body"),
                );
                let summary = bounds("git-summary");
                let message = bounds("git-commit-message");
                let notes = [0usize, 1].map(|ix| window.find(("commit-note", ix + 1)).bounds());
                let inside = |b: gpui_kit::Bounds<gpui_kit::Pixels>| {
                    gpui_kit::point(b.left() + px(2.), b.bottom() - px(2.))
                };

                assert_eq!(header.size.height, super::HEADER_HEIGHT, "{mode:?}: header");
                assert_eq!(
                    (header.top(), body.top()),
                    (panel.top(), header.bottom()),
                    "{mode:?}: the header and body aren't one after the other"
                );
                assert_eq!(
                    frame.at(inside(header)),
                    palette.ribbon_tabs,
                    "{mode:?}: header"
                );
                assert_eq!(frame.at(inside(body)), palette.ribbon, "{mode:?}: body");
                // No line between the header and the body.
                let x = header.left() + px(40.);
                for dy in [-2., -1.] {
                    let y = header.bottom() + px(dy + 0.5);
                    assert_eq!(
                        frame.at(gpui_kit::point(x, y)),
                        palette.ribbon_tabs,
                        "{mode:?}"
                    );
                }
                assert_eq!(
                    frame.at(gpui_kit::point(x, header.bottom() + px(0.5))),
                    palette.ribbon,
                    "{mode:?}: a line lies between the header and the body"
                );

                // Padded at either side and the bottom, 4 apart.
                for (name, b) in [
                    ("summary", summary),
                    ("note", notes[0]),
                    ("message", message),
                ] {
                    assert_eq!(
                        (b.left() - body.left(), body.right() - b.right()),
                        (super::PADDING, super::PADDING),
                        "{mode:?}: the {name} isn't inset 8px"
                    );
                }
                assert_eq!(summary.top(), body.top(), "{mode:?}: summary");
                assert_eq!(notes[0].top() - summary.bottom(), super::GAP, "{mode:?}");
                assert_eq!(notes[1].top() - notes[0].bottom(), super::GAP, "{mode:?}");
                assert_eq!(message.top() - notes[1].bottom(), super::GAP, "{mode:?}");
                let commit = bounds("git-commit");
                assert_eq!(body.bottom() - commit.bottom(), super::PADDING, "{mode:?}");

                // The notes on the tab row's colour, the long one wrapping
                // onto more lines than the short one, and the message on the
                // darkest surface.
                for note in notes {
                    assert_eq!(
                        frame.at(inside(note)),
                        palette.ribbon_tabs,
                        "{mode:?}: note"
                    );
                }
                assert!(
                    notes[0].size.height > notes[1].size.height,
                    "{mode:?}: the long note doesn't wrap: {notes:?}"
                );
                assert!(
                    notes[1].size.height <= px(28.),
                    "{mode:?}: a one-line note is {:?} tall",
                    notes[1].size.height
                );
                assert_eq!(
                    frame.at(inside(message)),
                    palette.darkest,
                    "{mode:?}: message"
                );

                // Every box and button square.
                for quad in window.painted_quads() {
                    let scale = window.scale_factor();
                    let (x, y) = (
                        quad.bounds.origin.x.0 / scale,
                        quad.bounds.origin.y.0 / scale,
                    );
                    if quad.background.as_solid().is_some_and(|c| c.a > 0.)
                        && x >= body.left().as_f32() - 0.5
                        && y >= header.top().as_f32() - 0.5
                        && y < body.bottom().as_f32()
                    {
                        assert!(
                            quad.corner_radii.top_left.0 == 0.
                                && quad.corner_radii.bottom_right.0 == 0.,
                            "{mode:?}: a rounded box in the panel at {:?}",
                            quad.bounds
                        );
                    }
                }
            })
            .unwrap();
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Generate, with a sparkles icon, on the tab row's colour, is at the left
    /// of the row beneath the message, and Commit, on the ribbon's button
    /// colour, at its right.
    #[gpui_kit::test]
    async fn generate_is_at_the_left_and_commit_at_the_right(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};
        use gpui_kit::px;
        let dir = repository_with_notes("git-panel-buttons");
        let (_, handle) = open_panel(&dir, cx).await;
        cx.update(|cx| Theme::change(ThemeMode::Dark, None, cx));
        settle(handle, cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let palette = crate::theme::palette(cx);
            let frame = crate::frame_image::Frame::of(window);
            let body = window.find("git-body").bounds();
            let message = window.find("git-commit-message").bounds();
            let generate = window.find("git-generate-message").bounds();
            let commit = window.find("git-commit").bounds();
            assert_eq!(
                generate.left() - body.left(),
                super::PADDING,
                "Generate isn't at the left"
            );
            assert_eq!(
                body.right() - commit.right(),
                super::PADDING,
                "Commit isn't at the right"
            );
            assert_eq!(generate.top(), commit.top(), "they aren't on one row");
            assert_eq!(
                generate.top() - message.bottom(),
                super::GAP,
                "not beneath the message"
            );
            assert!(generate.right() < commit.left());
            let middle = |b: gpui_kit::Bounds<gpui_kit::Pixels>| {
                gpui_kit::point(b.left() + px(1.5), b.top() + px(1.5))
            };
            assert_eq!(
                frame.at(middle(generate)),
                palette.ribbon_tabs,
                "Generate's colour"
            );
            assert_eq!(frame.at(middle(commit)), 0x444444, "Commit's colour");
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Commit notes saved with the project show as rows that can be removed,
    /// go into the commit beneath the message, and are cleared once
    /// committed.
    #[gpui_kit::test]
    async fn commit_notes_go_into_the_commit(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("suspense-panel-notes-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.name", "Test"]);
        git(&dir, &["config", "user.email", "test@example.com"]);
        std::fs::write(dir.join("readme.md"), "hello\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("readme.md"), "hello again\n").unwrap();
        for note in ["Add the ribbon", "Drop this one", "Fix scrolling"] {
            crate::commit_notes::add(&dir, note).unwrap();
        }

        cx.update(|cx| {
            gpui_kit::init(cx);
            piton_syntax::init();
            ProjectDirectory::init(cx);
            ProjectDirectory::set(dir.clone(), cx);
        });
        let mut panel = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| GitPanel::new(window, cx));
            panel = Some(view.clone());
            Root::new(view, window, cx)
        });
        let panel = panel.unwrap();
        let handle = window.into();
        cx.wait_for(handle, Duration::from_secs(5), |window, _| {
            window.try_find(("remove-commit-note", 2usize)).is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.click(("remove-commit-note", 2usize), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(panel.read_with(cx, |panel, _| panel.notes.len()), 2);

        cx.update_window(handle, |_, window, cx| {
            panel.update(cx, |panel, cx| {
                panel.set_message("Tidy the UI", window, cx);
                panel.commit(window, cx);
            });
        })
        .unwrap();
        let start = std::time::Instant::now();
        while !git(&dir, &["log", "-1", "--format=%s"]).contains("Tidy the UI") {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "nothing was committed"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.run_until_parked();
        assert_eq!(
            git(&dir, &["log", "-1", "--format=%B"]).trim(),
            "Tidy the UI\n\n- Add the ribbon\n- Fix scrolling"
        );
        assert!(
            crate::commit_notes::load(&dir).is_empty(),
            "the notes were not cleared"
        );
        assert!(
            !git(&dir, &["show", "--name-only", "--format=", "HEAD"]).contains("commit-notes"),
            "the notes' own file was committed"
        );
        assert!(panel.read_with(cx, |panel, _| panel.notes.is_empty()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
