//! Planning, as the PlanningScope says: a plain, quick list of the work
//! still to do, as Linear keeps issues. Each story is written as it will be
//! sent, a title, a prompt, and its mode, and the stories are gathered into
//! the next sprint and sent to the agent in order, all at once.
//!
//! The stories are kept with the project's data, in `.suspense/planning/`: a
//! file per story holding its hidden anchor as a queued prompt's is, with
//! its mode, model, effort, Slice, attachments, and steps, and `order.json`
//! listing the Draft, Next sprint, Backlog, and Done sections and their
//! stories in order, with each story's title, its marks, and the task it
//! was sent as, and the proposals still to be decided.
//!
//! The agent never writes the plan: drafting runs read the project and
//! change nothing, and what they propose waits as a [`Proposal`] until it is
//! accepted, as the PlanningScope's proposals say.
//!
//! [`Planning`] is the view, configured by [`PlanningHooks`]: what sending a
//! story does, how a sent story's task stands, opening and resuming that
//! task, and running the harness are the application's, so the view knows
//! nothing of the queue it sends to.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    CompletionProvider, Editor, EditorState, Enter, Escape, Input, InputEvent, InputState,
    MoveDown, MoveUp, Textarea, TextareaState,
};
use gpui_kit::component::menu::{
    ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::attached_file;
use crate::attached_image;
use crate::chat_input::{self, SendMode};
use crate::completion_menu::CompletionMenu;
use crate::divergence::{Cancel, RunAgent};
use crate::growing_input::GrowToFit;
use crate::hidden_anchor::{APP_DIR, AddedStep, Attached, HiddenAnchor};
use crate::piton_syntax;

actions!(planning, [OpenPlanning, NewStory, SendSprint]);

// Planning's own shortcuts, beyond its rows: opening Draft stories and
// Research, moving between its regions, toggling the switch focused, and
// opening the citation focused.
actions!(
    planning_keys,
    [
        OpenDraftStories,
        OpenResearch,
        NextRegion,
        ToggleSwitch,
        OpenCitation,
        OpenComposer
    ]
);

// The keys of a focused row or proposal, as the PlanningScope's keyboard
// says.
actions!(
    planning_row,
    [
        PreviousRow,
        NextRow,
        PreviousColumn,
        NextColumn,
        ToggleStory,
        MoveStoryUp,
        MoveStoryDown,
        MoveStoryLeft,
        MoveStoryRight,
        ToggleSprint,
        PickMode,
        DeleteStory,
        ToggleSelected,
        AcceptProposal,
        EditProposal,
        RejectProposal
    ]
);

/// The key context Planning's own shortcuts work in.
pub(crate) const CONTEXT: &str = "Planning";

/// The key context of a focused row.
const ROW_CONTEXT: &str = "PlanningRow";

/// The key context of a focused proposal.
const PROPOSAL_CONTEXT: &str = "PlanningProposal";

/// The key context of a focused step of the open story.
const STEP_CONTEXT: &str = "PlanningStep";

/// What adding a step to a story that doesn't exist says, as the
/// PlanningScope's steps say.
pub const NO_SUCH_STORY: &str = "That story no longer exists, so the step wasn't added.";

/// What adding a step to a story already sent says.
pub const STORY_SENT: &str = "That story has been sent, so the step wasn't added.";

/// The secondary modifier as a tooltip names it.
#[cfg(target_os = "macos")]
const MOD: &str = "⌘";
#[cfg(not(target_os = "macos"))]
const MOD: &str = "Ctrl+";

/// Planning's shortcuts: Ctrl+N (Cmd+N on macOS) adds a story and
/// Ctrl+Shift+Enter sends the sprint while Planning has focus; on a focused
/// row, Up and Down, as J and K, move between the rows of its column and
/// Left and Right to the column beside, Alt with them moves the story,
/// Enter opens it, Ctrl+Enter moves it between the lists, M picks its mode,
/// X toggles it in or out of the selection, and Delete deletes it once
/// confirmed, the same keys acting on the card under the pointer while no
/// card has focus; on a focused proposal, A accepts it, E edits it, and R
/// rejects it.
pub fn bind_keys(cx: &mut App) {
    let row = Some(ROW_CONTEXT);
    let proposal = Some(PROPOSAL_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("secondary-n", NewStory, Some(CONTEXT)),
        KeyBinding::new("secondary-shift-enter", SendSprint, Some(CONTEXT)),
        KeyBinding::new("secondary-shift-d", OpenDraftStories, Some(CONTEXT)),
        KeyBinding::new("secondary-shift-r", OpenResearch, Some(CONTEXT)),
        KeyBinding::new("f6", NextRegion, Some(CONTEXT)),
        KeyBinding::new("space", ToggleSwitch, Some("PlanningSwitch")),
        KeyBinding::new("enter", OpenCitation, Some("PlanningCitation")),
        KeyBinding::new("enter", OpenComposer, Some("PlanningAdd")),
        KeyBinding::new("space", OpenComposer, Some("PlanningAdd")),
        KeyBinding::new("secondary-enter", ToggleSprint, row),
        KeyBinding::new("up", PreviousRow, row),
        KeyBinding::new("down", NextRow, row),
        KeyBinding::new("k", PreviousRow, row),
        KeyBinding::new("j", NextRow, row),
        KeyBinding::new("left", PreviousColumn, row),
        KeyBinding::new("right", NextColumn, row),
        KeyBinding::new("enter", ToggleStory, row),
        KeyBinding::new("alt-up", MoveStoryUp, row),
        KeyBinding::new("alt-down", MoveStoryDown, row),
        KeyBinding::new("alt-left", MoveStoryLeft, row),
        KeyBinding::new("alt-right", MoveStoryRight, row),
        KeyBinding::new("m", PickMode, row),
        KeyBinding::new("x", ToggleSelected, row),
        KeyBinding::new("delete", DeleteStory, row),
        #[cfg(target_os = "macos")]
        KeyBinding::new("backspace", DeleteStory, row),
        // With no card focused, the card under the pointer.
        KeyBinding::new("enter", ToggleStory, Some(CONTEXT)),
        KeyBinding::new("up", PreviousRow, Some(CONTEXT)),
        KeyBinding::new("down", NextRow, Some(CONTEXT)),
        KeyBinding::new("x", ToggleSelected, Some(CONTEXT)),
        KeyBinding::new("m", PickMode, Some(CONTEXT)),
        KeyBinding::new("secondary-enter", ToggleSprint, Some(CONTEXT)),
        KeyBinding::new("delete", DeleteStory, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("backspace", DeleteStory, Some(CONTEXT)),
        KeyBinding::new("alt-up", MoveStoryUp, Some(CONTEXT)),
        KeyBinding::new("alt-down", MoveStoryDown, Some(CONTEXT)),
        KeyBinding::new("alt-left", MoveStoryLeft, Some(CONTEXT)),
        KeyBinding::new("alt-right", MoveStoryRight, Some(CONTEXT)),
        KeyBinding::new("up", PreviousRow, proposal),
        KeyBinding::new("down", NextRow, proposal),
        KeyBinding::new("k", PreviousRow, proposal),
        KeyBinding::new("j", NextRow, proposal),
        KeyBinding::new("left", PreviousColumn, proposal),
        KeyBinding::new("right", NextColumn, proposal),
        KeyBinding::new("a", AcceptProposal, proposal),
        KeyBinding::new("e", EditProposal, proposal),
        KeyBinding::new("r", RejectProposal, proposal),
        KeyBinding::new("up", PreviousRow, Some("PlanningBulkBar")),
        KeyBinding::new("down", NextRow, Some("PlanningBulkBar")),
        KeyBinding::new("enter", ToggleStory, Some("PlanningBulkBar")),
        KeyBinding::new("alt-up", MoveStoryUp, Some(STEP_CONTEXT)),
        KeyBinding::new("alt-down", MoveStoryDown, Some(STEP_CONTEXT)),
    ]);
}

/// Where the project's stories are kept.
const PLANNING_DIR: &str = "planning";

/// The file listing the sections and their stories.
const ORDER_FILE: &str = "order.json";

/// How wide a finding is read in its tab, at most.
const READING_WIDTH: Pixels = px(760.);

/// How wide each of the board's columns is.
const COLUMN_WIDTH: Pixels = px(320.);

/// How far apart the board's columns are.
const COLUMN_GAP: Pixels = px(16.);

/// How far apart the cards in a column are.
const CARD_GAP: Pixels = px(8.);

/// How near a dragged card comes to an edge of the board, or of a column,
/// before it scrolls.
const DRAG_EDGE: Pixels = px(32.);

/// The fastest the board or a column scrolls while a card is held at or
/// past its edge, in pixels a millisecond: about a row every 50
/// milliseconds.
const DRAG_MOST_SPEED: f32 = 36. / 50.;

/// How often the board or a column scrolls on while a card is held near
/// its edge: each frame.
const DRAG_FRAME: Duration = Duration::from_millis(16);

/// How tall a story's row is.
const ROW_HEIGHT: Pixels = px(36.);

/// The most rows a story's prompt grows to before it scrolls.
const PROMPT_MAX_ROWS: usize = 12;

/// The most rows the box saying what is wanted grows to.
const DRAFT_MAX_ROWS: usize = 6;

/// How many lines of a proposal's prompt its card shows until read more.
const PROPOSAL_LINES: usize = 4;

/// How long a rejected proposal can be brought back.
const UNDO_FOR: Duration = Duration::from_secs(5);

/// A section of the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    /// The sprint sent, while it holds a story.
    Current,
    Next,
    Backlog,
    Done,
    /// Rough stories, not ready to plan yet.
    Draft,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Current => "Current sprint",
            Section::Next => "Next sprint",
            Section::Backlog => "Backlog",
            Section::Done => "Done",
            Section::Draft => "Draft",
        }
    }

    /// What its empty state says: what is missing, and what to do.
    fn empty(self) -> String {
        match self {
            Section::Next => format!("Nothing planned. Press {MOD}N to add a story."),
            Section::Backlog => "Nothing waiting.".to_string(),
            Section::Done => "Nothing done yet.".into(),
            Section::Current => "No sprint under way. Send sprint to start one.".into(),
            Section::Draft => "No drafts. Jot an idea down here.".into(),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Section::Current => "current",
            Section::Next => "next",
            Section::Backlog => "backlog",
            Section::Done => "done",
            Section::Draft => "draft",
        }
    }
}

/// A story's mode as its pill reads it.
pub fn mode_label(mode: SendMode) -> &'static str {
    match mode {
        SendMode::Code => "Code",
        SendMode::Both => "Chain",
        SendMode::Spec => "Spec",
        SendMode::Ask => "Ask",
        SendMode::Freeform => "Freeform",
    }
}

/// A mode as a `suspense-story` block names it.
pub fn mode_slug(mode: SendMode) -> &'static str {
    match mode {
        SendMode::Code => "code",
        SendMode::Both => "chain",
        SendMode::Spec => "spec",
        SendMode::Ask => "ask",
        SendMode::Freeform => "freeform",
    }
}

/// The mode a `suspense-story` block names.
pub fn mode_of_slug(slug: &str) -> Option<SendMode> {
    match slug.trim().to_ascii_lowercase().as_str() {
        "code" => Some(SendMode::Code),
        "chain" | "combined" | "both" => Some(SendMode::Both),
        "spec" => Some(SendMode::Spec),
        "ask" => Some(SendMode::Ask),
        "freeform" => Some(SendMode::Freeform),
        _ => None,
    }
}

/// The mode last picked for a story, which a new one takes.
static LAST_MODE: Mutex<Option<SendMode>> = Mutex::new(None);

fn last_mode() -> SendMode {
    LAST_MODE
        .lock()
        .ok()
        .and_then(|mode| *mode)
        .unwrap_or(SendMode::Both)
}

/// One piece of work, written as it will be sent.
pub struct Story {
    /// What it is known by: its hidden anchor's name, and its file's.
    pub name: String,
    pub title: String,
    /// Its prompt, as written; empty to send its title.
    pub prompt: String,
    /// Its mode, model, effort, Slice, attachments, and steps.
    pub anchor: HiddenAnchor,
    /// Accepted from the agent and not yet opened.
    pub from_agent: bool,
    /// Given a step by the agent and not opened since.
    pub updated: bool,
    /// The step the agent added last, shown highlighted the first time the
    /// story is opened after.
    pub highlight: Option<usize>,
    /// The spec elements it touches, as the PlanningScope's spec grounding
    /// says.
    pub spec: Vec<Citation>,
    /// The task it was last sent as, by name.
    pub task: Option<String>,
    /// How many stories its drafts in Draft added to the Backlog, its card
    /// marked "Drafted" once there are any; kept in its file.
    pub drafted: usize,
}

impl Story {
    /// A new story of `mode`, titled `title`, with the model and effort
    /// chosen now for the project's harness.
    pub fn new(title: String, prompt: String, mode: SendMode, project_dir: &Path) -> Self {
        let mut anchor = HiddenAnchor::random();
        anchor.rename(story_name());
        anchor.mode = Some(mode);
        let agent = crate::agent::of_project(Some(project_dir));
        anchor.model = crate::models::chosen(agent);
        anchor.effort = crate::effort::chosen(agent);
        Self {
            name: anchor.name().to_string(),
            title,
            prompt,
            anchor,
            from_agent: false,
            updated: false,
            highlight: None,
            spec: Vec::new(),
            task: None,
            drafted: 0,
        }
    }

    pub fn mode(&self) -> SendMode {
        self.anchor.mode.unwrap_or(SendMode::Both)
    }

    /// What it sends: its prompt, or its title where it has none.
    pub fn text(&self) -> String {
        if self.prompt.trim().is_empty() {
            self.title.clone()
        } else {
            self.prompt.clone()
        }
    }

    /// Its steps, after its own prompt.
    pub fn steps(&self) -> &[AddedStep] {
        &self.anchor.added_steps
    }

    /// Whether anything is attached to it.
    fn has_attachments(&self) -> bool {
        !self.anchor.attached_text.is_empty()
            || !self.anchor.attached_images.is_empty()
            || !self.anchor.attached_files.is_empty()
    }

    /// A copy of it under a new name, its steps with it, never sent.
    fn duplicate(&self) -> Self {
        let source = self.anchor.source(&self.prompt);
        let mut anchor = HiddenAnchor::parse(&source)
            .map(|(anchor, _)| anchor)
            .unwrap_or_else(HiddenAnchor::random);
        anchor.rename(story_name());
        Self {
            name: anchor.name().to_string(),
            title: self.title.clone(),
            prompt: self.prompt.clone(),
            anchor,
            from_agent: false,
            updated: false,
            highlight: None,
            spec: self.spec.clone(),
            task: None,
            drafted: 0,
        }
    }

    /// Its file's source: its hidden anchor, and its "Drafted" mark where
    /// it has one.
    fn source(&self) -> String {
        let mut source = self.anchor.source(&self.prompt);
        if self.drafted > 0 {
            source.push_str(&format!("\n{DRAFTED_PREFIX}{}\n", self.drafted));
        }
        source
    }
}

/// The start of the line of a story's file keeping its "Drafted" mark: how
/// many stories its drafts added to the Backlog.
const DRAFTED_PREFIX: &str = "    drafted: ";

/// The "Drafted" mark a story's file keeps, none read as 0.
fn drafted_of(source: &str) -> usize {
    source
        .lines()
        .find_map(|line| line.strip_prefix(DRAFTED_PREFIX)?.trim().parse().ok())
        .unwrap_or(0)
}

/// A fresh name for a story's anchor and file.
fn story_name() -> String {
    HiddenAnchor::random_name().replacen("Prompt_", "Story_", 1)
}

/// A story as it is sent: the name its task takes, so its status can be
/// followed, and everything it is sent with.
pub struct StorySend {
    pub name: String,
    pub text: String,
    pub mode: SendMode,
    pub attached: Attached,
    pub sliced: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Its steps, sent after it in order.
    pub steps: Vec<AddedStep>,
}

/// How a sent story's task stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoryState {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Cancelled,
    /// Its task can't be found: its record deleted, never recorded, or lost
    /// as the application quit. Ended, and not Done.
    Missing,
}

impl StoryState {
    pub(crate) fn ended(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Cancelled | Self::Missing
        )
    }

    fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::Missing => "Task missing",
        }
    }
}

/// What kind of spec element a citation is, as the PlanningScope's spec
/// grounding says, shown as a small muted label before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CitationKind {
    Requirement,
    Behaviour,
    Name,
    Section,
}

impl CitationKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Requirement => "Requirement",
            Self::Behaviour => "Behaviour",
            Self::Name => "Name",
            Self::Section => "Section",
        }
    }

    /// The kind a word names, as a `spec` entry begins with it.
    fn of_word(word: &str) -> Option<Self> {
        let word = word.trim().trim_end_matches(':').to_ascii_lowercase();
        match word.as_str() {
            "requirement" => Some(Self::Requirement),
            "behaviour" | "behavior" => Some(Self::Behaviour),
            "name" => Some(Self::Name),
            "section" => Some(Self::Section),
            _ => None,
        }
    }
}

/// A spec element a story cites, as the PlanningScope's spec grounding
/// says: a link to the element in the spec's Piton source, its file and the
/// anchor or property it names, as `spec/scope/planning/index.pi#Planning.sprint`,
/// with its kind, what in it the story touches, and the few words of the
/// spec it rests on; or, with no link, "No match", with what was looked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default)]
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CitationKind>,
    /// The words of the spec it rests on, quoted.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote: String,
}

impl Citation {
    /// A citation of `link`, picked by hand, with nothing more said of it.
    pub fn of_link(link: String) -> Self {
        Self {
            link: Some(link),
            note: String::new(),
            kind: None,
            quote: String::new(),
        }
    }

    /// The citation a `spec` list's entry holds: its kind first, where it
    /// names one; then `No match — what was looked for`, a markdown link, or
    /// a link and what it touches; and the words it rests on, in quotes, at
    /// its end.
    pub fn parse(entry: &str) -> Option<Self> {
        let entry = entry.trim().trim_start_matches(['-', '*']).trim();
        if entry.is_empty() {
            return None;
        }
        let rest = |text: &str| {
            text.trim()
                .trim_start_matches(['—', '–', '-', ':', ','])
                .trim()
                .to_string()
        };
        // Its kind, where it begins with one.
        let (kind, entry) = match entry.split_once(char::is_whitespace) {
            Some((word, after)) if CitationKind::of_word(word).is_some() => {
                (CitationKind::of_word(word), after.trim())
            }
            _ => (None, entry),
        };
        let (link, note) = if entry.to_ascii_lowercase().starts_with("no match") {
            (None, rest(&entry["no match".len()..]))
        } else if let Some(open) = entry.strip_prefix('[')
            && let Some((name, after)) = open.split_once("](")
            && let Some((link, note)) = after.split_once(')')
        {
            let note = rest(note);
            (
                Some(link.trim().to_string()),
                if note.is_empty() {
                    name.to_string()
                } else {
                    note
                },
            )
        } else {
            let (link, note) = entry.split_once(char::is_whitespace).unwrap_or((entry, ""));
            (
                Some(
                    link.trim_matches(['`', '<', '>'])
                        .trim_end_matches(':')
                        .to_string(),
                ),
                rest(note),
            )
        };
        let (note, quote) = split_quote(&note);
        Some(Self {
            link,
            note: rest(note.trim_end().trim_end_matches(['—', '–', '-', ','])),
            kind,
            quote,
        })
    }

    /// The element it names, as `Planning.sprint`, or its file's name where
    /// it names none; "No match" for none.
    pub fn label(&self) -> String {
        let Some(link) = &self.link else {
            return "No match".into();
        };
        match link.split_once('#') {
            Some((_, element)) if !element.is_empty() => element.to_string(),
            _ => Path::new(link)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| link.clone()),
        }
    }

    /// The file it links, relative to the project.
    pub fn file(&self) -> Option<&str> {
        let link = self.link.as_deref()?;
        Some(link.split('#').next().unwrap_or(link))
    }
}

/// `note` with the words in quotes at its end taken out, and those words.
fn split_quote(note: &str) -> (String, String) {
    let trimmed = note.trim_end();
    for (open, close) in [('"', '"'), ('“', '”')] {
        if let Some(body) = trimmed.strip_suffix(close)
            && let Some(start) = body.rfind(open)
        {
            let quote = body[start + open.len_utf8()..].trim().to_string();
            if !quote.is_empty() {
                return (body[..start].to_string(), quote);
            }
        }
    }
    (note.to_string(), String::new())
}

/// The spec location, relative to the project, as citations are written
/// under it: `spec` where `piton.config.pi` names none.
pub fn spec_location(project_dir: &Path) -> String {
    crate::hidden_anchor::spec_dir(project_dir)
        .and_then(|dir| {
            dir.strip_prefix(project_dir)
                .ok()
                .map(|dir| dir.to_string_lossy().replace('\\', "/"))
        })
        .filter(|dir| !dir.is_empty())
        .unwrap_or_else(|| "spec".into())
}

/// The elements a Piton file declares, each with the line it is declared
/// on, from the top: its anchors, and the properties on them, as
/// `Planning` and `Planning.sprint`, as `piton slice` names them.
pub fn declared_elements(text: &str) -> Vec<(String, usize)> {
    // The element each open block belongs to, its indent, and the indent
    // its own properties are at, once seen.
    let mut open: Vec<(usize, String, Option<usize>)> = Vec::new();
    let mut found = Vec::new();
    for (at, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        if code.trim().is_empty() {
            continue;
        }
        let indent = code.len() - code.trim_start().len();
        if indent == 0 {
            open.clear();
            if let Some((_, name)) = crate::spec_components::declared_names(code)
                .into_iter()
                .next()
            {
                found.push((name.clone(), at));
                open.push((0, name, None));
            }
            continue;
        }
        while open
            .last()
            .is_some_and(|(at_indent, ..)| *at_indent >= indent)
        {
            open.pop();
        }
        let Some((_, parent, child)) = open.last_mut() else {
            continue;
        };
        // Only lines at its properties' own indent are properties of it;
        // deeper ones are their text.
        let child = *child.get_or_insert(indent);
        if child != indent {
            continue;
        }
        let word: String = code
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        let after = code.trim_start()[word.len()..].trim_start();
        if word.is_empty() || !after.starts_with(':') {
            continue;
        }
        let name = format!("{parent}.{word}");
        found.push((name.clone(), at));
        open.push((indent, name, None));
    }
    found
}

/// Whether `link`, a citation's, points at an element the spec's Piton
/// source defines, as the PlanningScope's spec grounding says: its file
/// under the spec location of the project at `project_dir`, and the anchor
/// or property it names declared in it. Gives the line it is declared on,
/// or why not.
pub fn check_link(link: &str, project_dir: &Path) -> Result<Option<usize>, String> {
    let (path, element) = link.split_once('#').unwrap_or((link, ""));
    let path = path.trim_start_matches("./");
    let under = Path::new(path).starts_with(spec_location(project_dir))
        && path.ends_with(".pi")
        && !Path::new(path)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir));
    let text = under
        .then(|| std::fs::read_to_string(project_dir.join(path)).ok())
        .flatten()
        .ok_or_else(|| "no such file".to_string())?;
    if element.is_empty() {
        return Ok(None);
    }
    declared_elements(&text)
        .into_iter()
        .find(|(name, _)| name == element)
        .map(|(_, at)| Some(at))
        .ok_or_else(|| format!("the file doesn't define {element}"))
}

/// Every element the spec's Piton source declares, as the link a citation
/// takes and the element's name, found from the files under the spec
/// location, for "Add citation" to search by name.
pub fn spec_elements(project_dir: &Path) -> Vec<(String, String)> {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "pi") {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&project_dir.join(spec_location(project_dir)), &mut files);
    let mut elements = Vec::new();
    for file in files {
        let (Ok(text), Ok(relative)) = (
            std::fs::read_to_string(&file),
            file.strip_prefix(project_dir),
        ) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        for (name, _) in declared_elements(&text) {
            elements.push((format!("{relative}#{name}"), name));
        }
    }
    elements
}

/// Opens the Piton source where `link` points, at its element's
/// declaration where the file still defines it.
fn open_citation(
    link: &str,
    project_dir: &Path,
    open: &OpenSource,
    window: &mut Window,
    cx: &mut App,
) {
    let path = link.split('#').next().unwrap_or(link);
    let line = check_link(link, project_dir).ok().flatten();
    open(project_dir.join(path), line, window, cx)
}

/// Checks each link `proposal` cites against the Piton source of the
/// project at `project_dir`, as it arrives from drafting, research, or an Ask
/// answer alike: any that fails is dropped, with a note of which and why.
pub fn check_citations(proposal: &mut Proposal, project_dir: &Path) {
    let mut dropped = Vec::new();
    proposal.spec.retain(|citation| {
        let Some(link) = &citation.link else {
            return true;
        };
        match check_link(link, project_dir) {
            Ok(_) => true,
            Err(why) => {
                dropped.push(format!("Dropped {link}: {why}."));
                false
            }
        }
    });
    proposal.dropped.extend(dropped);
}

/// A task the agent proposes, as the PlanningScope's proposals say: its
/// title, prompt, and mode, and where it goes, a new story or a step of an
/// existing one, waiting to be accepted, edited, or rejected.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    /// What it is known by while it waits.
    pub id: String,
    pub title: String,
    pub prompt: String,
    /// Its mode, as a `suspense-story` block names it.
    pub mode: String,
    /// The story it is to be added to, by name; none for a new story.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Its block's target was `rewrite`, as drafts gave before they proposed
    /// only new stories: read as a new story.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rewrite: bool,
    /// Its block's target was `step`: the follow-up step of the story just
    /// before it, never a proposal of its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub follow_up: bool,
    /// It came from a draft about a story, so it is only ever a new story,
    /// never one added to the story drafted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drafted: bool,
    /// The new story's Spec follow-up step, its own step once accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<FollowUp>,
    /// Why it is a new story though another was named, shown muted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The story the drafting run was asked about, beneath which it shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    /// The spec elements it touches, as found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spec: Vec<Citation>,
    /// The citations dropped as it arrived, and why, shown muted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<String>,
}

impl Proposal {
    pub fn mode(&self) -> SendMode {
        mode_of_slug(&self.mode).unwrap_or(SendMode::Both)
    }

    /// What a new story made from it is titled: its title, or its prompt's
    /// first line.
    fn story_title(&self) -> String {
        if self.title.trim().is_empty() {
            self.prompt
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        } else {
            self.title.trim().to_string()
        }
    }
}

/// A proposed new story's follow-up step: its mode and prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FollowUp {
    pub mode: String,
    pub prompt: String,
}

impl FollowUp {
    pub fn mode(&self) -> SendMode {
        mode_of_slug(&self.mode).unwrap_or(SendMode::Spec)
    }
}

/// The proposals of a draft about the story `about`, each a new story, as
/// planning.pi's proposals say: a block targeting the story drafted, by its
/// id or as `rewrite`, reads as `new`; one that is a step, its target `step`
/// or a step added to the story drafted, is the follow-up step of the new
/// story just before it, or, where there is none, a new story of its own.
pub fn draft_proposals(about: &str, proposals: Vec<Proposal>) -> Vec<Proposal> {
    let mut stories: Vec<Proposal> = Vec::new();
    for mut proposal in proposals {
        let adds_step = proposal.follow_up
            || (!proposal.rewrite && proposal.target.as_deref() == Some(about));
        if adds_step
            && let Some(story) = stories.last_mut()
            && story.step.is_none()
        {
            story.step = Some(FollowUp {
                mode: proposal.mode.clone(),
                prompt: proposal.prompt.clone(),
            });
            for citation in proposal.spec {
                if !story.spec.contains(&citation) {
                    story.spec.push(citation);
                }
            }
            story.dropped.extend(proposal.dropped);
            continue;
        }
        proposal.target = None;
        proposal.rewrite = false;
        proposal.follow_up = false;
        proposal.drafted = true;
        proposal.about = Some(about.to_string());
        stories.push(proposal);
    }
    stories
}

/// A fresh id for a proposal.
fn proposal_id() -> String {
    HiddenAnchor::random_name().replacen("Prompt_", "Proposal_", 1)
}

/// The proposal a `suspense-story` block holds: a line each for its title,
/// mode, and target, `new`, a story's id, or `step`, a follow-up step,
/// then, after a blank line, its prompt. None for a block with neither title nor prompt.
pub fn parse_story_block(body: &str) -> Option<Proposal> {
    let mut proposal = Proposal {
        id: proposal_id(),
        title: String::new(),
        prompt: String::new(),
        mode: "chain".into(),
        target: None,
        rewrite: false,
        follow_up: false,
        drafted: false,
        step: None,
        note: None,
        about: None,
        spec: Vec::new(),
        dropped: Vec::new(),
    };
    let mut lines = body.lines();
    let mut prompt: Vec<&str> = Vec::new();
    let mut in_spec = false;
    for line in lines.by_ref() {
        if line.trim().is_empty() {
            break;
        }
        // The spec list: its entries, one per line, each begun with a dash.
        if in_spec && line.trim_start().starts_with(['-', '*']) {
            proposal.spec.extend(Citation::parse(line));
            continue;
        }
        in_spec = false;
        let Some((key, value)) = line.split_once(':') else {
            prompt.push(line);
            break;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "title" => proposal.title = value.to_string(),
            "mode" => {
                if let Some(mode) = mode_of_slug(value) {
                    proposal.mode = mode_slug(mode).into();
                }
            }
            "target" => {
                proposal.rewrite = value.eq_ignore_ascii_case("rewrite");
                proposal.follow_up = value.eq_ignore_ascii_case("step");
                proposal.target = (!value.is_empty()
                    && !value.eq_ignore_ascii_case("new")
                    && !proposal.rewrite
                    && !proposal.follow_up)
                    .then(|| value.to_string());
            }
            "spec" => {
                in_spec = true;
                proposal.spec.extend(Citation::parse(value));
            }
            "prompt" => {
                if !value.is_empty() {
                    prompt.push(value);
                }
                break;
            }
            _ => {
                prompt.push(line);
                break;
            }
        }
    }
    prompt.extend(lines);
    proposal.prompt = prompt.join("\n").trim().to_string();
    (!proposal.title.trim().is_empty() || !proposal.prompt.is_empty()).then_some(proposal)
}

/// The proposals a reply holds, each a `suspense-story` block, in order,
/// whatever else it says.
pub fn parse_proposals(reply: &str) -> Vec<Proposal> {
    crate::answer_blocks::split(reply)
        .into_iter()
        .filter_map(|part| match part {
            crate::answer_blocks::AnswerPart::Story { proposal, .. } => proposal,
            _ => None,
        })
        .collect()
}

/// How the task named so stands, and how long it has run while it runs.
pub type StateOf = Rc<dyn Fn(&str, &App) -> Option<(StoryState, Option<Duration>)>>;

/// How a step of the task named so stands, by its place among its steps.
pub type StepStateOf = Rc<dyn Fn(&str, usize, &App) -> Option<(StoryState, Option<Duration>)>>;

/// Sends stories, in order, as one block.
pub type SendStories = Rc<dyn Fn(Vec<StorySend>, &mut App)>;

/// Opens the task named so.
pub type OpenTask = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// Opens a file of the spec's Piton source, at the line given, where one is.
pub type OpenSource = Rc<dyn Fn(PathBuf, Option<usize>, &mut Window, &mut App)>;

/// Opens findings, by their file, titled.
pub type OpenFindings = Rc<dyn Fn(PathBuf, String, &mut Window, &mut App)>;

/// The completions a prompt offers.
pub type Completions = Rc<dyn Fn(&App) -> Rc<dyn CompletionProvider>>;

/// What the application does for Planning.
#[derive(Clone)]
pub struct PlanningHooks {
    /// How the task named so stands; none while it can't be found, as when
    /// it is still being queued.
    pub state: StateOf,
    /// How the step at a place of the task named so stands; none while it
    /// hasn't started.
    pub step_state: StepStateOf,
    /// Sends the stories, in order, as one block.
    pub send: SendStories,
    /// Opens the task named so in the Tasks tab.
    pub open_task: OpenTask,
    /// Resumes the paused task named so.
    pub resume: OpenTask,
    /// Runs the harness to propose stories, reading the project and
    /// changing nothing, each line it streams handed on as it comes.
    pub draft: RunAgent,
    /// Runs the harness to research a topic, reading the project and the
    /// web and changing nothing.
    pub research: RunAgent,
    /// Opens a piece of research's findings in a body tab of its own.
    pub open_findings: OpenFindings,
    /// Opens the spec's Piton source where a citation points, at the
    /// element's declaration.
    pub open_source: OpenSource,
    /// The completions a story's prompt offers, as the chat input's.
    pub completions: Completions,
}

/// Where the open project's sprint stands, for the ribbon's Send Sprint and
/// its Planning tab: how many stories wait in Next sprint, whether a sprint
/// is under way, and whether planning is paused for review.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SprintPending {
    pub waiting: usize,
    pub under_way: bool,
    pub paused: bool,
    /// Whether Start automatically is on.
    pub auto_start: bool,
}

impl Global for SprintPending {}

impl SprintPending {
    pub fn get(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }

    /// Why Send sprint can't send now, if it can't.
    pub fn held(self) -> Option<&'static str> {
        if self.paused {
            Some("Planning is paused for review: resume or approve it first")
        } else if self.under_way {
            Some("A sprint is already under way")
        } else if self.waiting == 0 {
            Some("Nothing in Next sprint is waiting to be sent")
        } else {
            None
        }
    }
}

// --- Saving -----------------------------------------------------------------

/// A story's entry in `order.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Entry {
    story: String,
    #[serde(default)]
    title: String,
    /// Accepted from the agent and not yet opened.
    #[serde(default, alias = "draft", skip_serializing_if = "std::ops::Not::not")]
    from_agent: bool,
    /// Given a step by the agent and not opened since.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    updated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    highlight: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    spec: Vec<Citation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task: Option<String>,
}

/// `order.json`: the sections and their stories, in order, and the
/// proposals still to be decided.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Order {
    /// Missing from a plan saved before Draft existed, read as empty.
    #[serde(default)]
    draft: Vec<Entry>,
    #[serde(default)]
    next: Vec<Entry>,
    #[serde(default)]
    backlog: Vec<Entry>,
    #[serde(default)]
    done: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    proposals: Vec<Proposal>,
    /// The sprint sent, while it holds a story.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    current: Vec<Entry>,
    /// The research done, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    findings: Vec<Finding>,
    /// The pause research left, until it is resumed or approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    paused: Option<Pause>,
    /// The Start automatically switch, off until turned on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    auto_start: bool,
}

/// How a piece of research ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingState {
    Done,
    Failed,
    Stopped,
}

impl FindingState {
    fn label(self) -> &'static str {
        match self {
            Self::Done => "Done",
            Self::Failed => "Failed",
            // Ended early, as stopped.
            Self::Stopped => "Incomplete",
        }
    }
}

/// A piece of research, as the PlanningScope's research says: its findings
/// are a markdown file of their own in `.suspense/planning/research/`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Its file's name in the research folder.
    pub file: String,
    pub topic: String,
    /// When it ran, in seconds since the epoch.
    pub ran_at: u64,
    pub state: FindingState,
    /// The story it was started from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story: Option<String>,
}

/// Planning paused for review once research ended, as the PlanningScope's
/// paused says: the research it waits on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pause {
    /// The findings' file.
    pub research: String,
    pub topic: String,
}

/// What the plan keeps beside its stories and proposals.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notes {
    pub findings: Vec<Finding>,
    pub paused: Option<Pause>,
    /// Next sprint starts by itself once the current one is done.
    pub auto_start: bool,
    /// Each story's own draft with the agent, as saved, by the story's
    /// name.
    pub drafts: std::collections::BTreeMap<String, SavedDraft>,
}

/// A story's own draft with the agent as its file in
/// `.suspense/planning/proposals/` keeps it: the proposals waiting to be
/// decided, and whether its run was going, so one cut off as the
/// application quit says so once it is open again.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedDraft {
    #[serde(default)]
    pub proposals: Vec<Proposal>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub running: bool,
}

/// What a story's card says of a run cut off as the application quit.
const CUT_OFF: &str = "The run was cut off when the application quit.";

/// Where each story's waiting proposals are kept, a file per story.
pub fn proposals_dir(project_dir: &Path) -> PathBuf {
    planning_dir(project_dir).join("proposals")
}

fn draft_file(project_dir: &Path, name: &str) -> PathBuf {
    proposals_dir(project_dir).join(format!("{name}.json"))
}

/// Each story's draft as saved, by its story's name, those that can't be
/// read left out.
fn load_drafts(project_dir: &Path) -> std::collections::BTreeMap<String, SavedDraft> {
    let Ok(entries) = std::fs::read_dir(proposals_dir(project_dir)) else {
        return Default::default();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let name = path.file_stem()?.to_str()?.to_string();
            let text = std::fs::read_to_string(&path).ok()?;
            let mut draft: SavedDraft = serde_json::from_str(&text).ok()?;
            // Saved before drafts proposed only new stories: read as they are.
            draft.proposals = draft_proposals(&name, std::mem::take(&mut draft.proposals));
            Some((name, draft))
        })
        .collect()
}

/// Where research findings are kept.
pub fn research_dir(project_dir: &Path) -> PathBuf {
    planning_dir(project_dir).join("research")
}

/// Where the project's stories are kept.
pub fn planning_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(PLANNING_DIR)
}

fn story_file(project_dir: &Path, name: &str) -> PathBuf {
    planning_dir(project_dir).join(format!("{name}.pi"))
}

/// The plan as saved: Next sprint, Backlog, Done, Current sprint, and
/// Draft, a plan saved before Draft existed reading it as empty; the
/// proposals waiting, and the findings and pause. A story's file not listed
/// goes at the end of the Backlog; one listed whose file is gone is left
/// out.
pub fn load(project_dir: &Path) -> ([Vec<Story>; 5], Vec<Proposal>, Notes) {
    load_checked(project_dir).unwrap_or_default()
}

/// The plan as saved: its sections, its proposals, and its notes.
type Plan = ([Vec<Story>; 5], Vec<Proposal>, Notes);

/// The plan as saved, as [`load`] gives it, or, where its `order.json` is
/// there but can't be read, which file and why.
fn load_checked(project_dir: &Path) -> Result<Plan, String> {
    let file = planning_dir(project_dir).join(ORDER_FILE);
    if let Ok(text) = std::fs::read_to_string(&file)
        && let Err(err) = serde_json::from_str::<Order>(&text)
    {
        return Err(format!(
            "{} couldn't be read: {err}",
            file.strip_prefix(project_dir).unwrap_or(&file).display()
        ));
    }
    Ok(load_order(project_dir))
}

/// The plan as saved, an unreadable order read as empty.
fn load_order(project_dir: &Path) -> ([Vec<Story>; 5], Vec<Proposal>, Notes) {
    let dir = planning_dir(project_dir);
    let order: Order = std::fs::read_to_string(dir.join(ORDER_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let read = |entry: &Entry| -> Option<Story> {
        let source = std::fs::read_to_string(story_file(project_dir, &entry.story)).ok()?;
        let (anchor, prompt) = HiddenAnchor::parse(&source)?;
        Some(Story {
            name: entry.story.clone(),
            title: entry.title.clone(),
            prompt,
            anchor,
            from_agent: entry.from_agent,
            updated: entry.updated,
            highlight: entry.highlight,
            spec: entry.spec.clone(),
            task: entry.task.clone(),
            drafted: drafted_of(&source),
        })
    };
    let mut listed = std::collections::HashSet::new();
    let order_sections = [order.next, order.backlog, order.done, order.current, order.draft];
    let mut sections = order_sections.map(|entries| {
        entries
            .iter()
            .filter(|entry| listed.insert(entry.story.clone()))
            .filter_map(read)
            .collect::<Vec<_>>()
    });
    let mut unlisted: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "pi"))
                .collect()
        })
        .unwrap_or_default();
    unlisted.sort();
    for path in unlisted {
        let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if listed.contains(name) {
            continue;
        }
        let entry = Entry {
            story: name.to_string(),
            title: String::new(),
            from_agent: false,
            updated: false,
            highlight: None,
            spec: Vec::new(),
            task: None,
        };
        if let Some(story) = read(&entry) {
            sections[1].push(story);
        }
    }
    // A story's waiting proposals go with it.
    let mut drafts = load_drafts(project_dir);
    drafts.retain(|name, _| sections.iter().flatten().any(|story| story.name == *name));
    let notes = Notes {
        findings: order.findings,
        auto_start: order.auto_start,
        paused: order.paused,
        drafts,
    };
    (sections, order.proposals, notes)
}

/// Saves a story's file, named `name`, holding `source`, its hidden
/// anchor's.
fn save_story(project_dir: &Path, name: &str, source: &str) -> Result<()> {
    let dir = planning_dir(project_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let file = story_file(project_dir, name);
    std::fs::write(&file, source).with_context(|| format!("could not save {}", file.display()))
}

/// The plan as `order.json` holds it.
fn order_of(sections: &[Vec<Story>; 5], proposals: &[Proposal], notes: &Notes) -> String {
    let entries = |stories: &Vec<Story>| {
        stories
            .iter()
            .map(|story| Entry {
                story: story.name.clone(),
                title: story.title.clone(),
                from_agent: story.from_agent,
                updated: story.updated,
                highlight: story.highlight,
                spec: story.spec.clone(),
                task: story.task.clone(),
            })
            .collect()
    };
    let order = Order {
        draft: entries(&sections[4]),
        next: entries(&sections[0]),
        backlog: entries(&sections[1]),
        done: entries(&sections[2]),
        proposals: proposals.to_vec(),
        current: entries(&sections[3]),
        findings: notes.findings.clone(),
        paused: notes.paused.clone(),
        auto_start: notes.auto_start,
    };
    serde_json::to_string_pretty(&order).unwrap_or_default()
}

fn save_order_text(project_dir: &Path, text: &str) -> Result<()> {
    let dir = planning_dir(project_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let file = dir.join(ORDER_FILE);
    std::fs::write(&file, text).with_context(|| format!("could not save {}", file.display()))
}

// --- Drafting -----------------------------------------------------------------

/// The example of a `suspense-story` block the request shows, its links
/// under `spec`, the spec location.
fn story_example(spec: &str) -> String {
    format!(
        "```suspense-story\ntitle: Close a finished sprint\nmode: chain\ntarget: new\nspec:\n- {spec}/scope/planning/index.pi#Planning.sprint: when Close sprint shows\n- No match: archiving a closed sprint\nprompt:\nClose sprint is offered once every story of the current sprint has\nended, as {spec}/scope/planning/index.pi#Planning.sprint says.\n```"
    )
}

/// The worked split of planning.pi, in a draft about a story: the story in
/// full, a chain, its Spec follow-up step, and a new story for the other
/// piece, each citing under `spec`.
fn split_example(spec: &str) -> String {
    format!(
        "```suspense-story\ntitle: Close a finished sprint\nmode: chain\ntarget: new\nspec:\n- {spec}/scope/planning/index.pi#Planning.sprint: when Close sprint shows\nprompt:\nClose sprint is offered once every story of the current sprint has\nended, as {spec}/scope/planning/index.pi#Planning.sprint says.\n```\n\n```suspense-story\ntitle: Settle when Close sprint shows\nmode: spec\ntarget: step\nspec:\n- {spec}/scope/planning/index.pi#Planning.sprint: when Close sprint shows\nprompt:\nSettle {spec}/scope/planning/index.pi#Planning.sprint with what the code\nnow does once every story of the current sprint has ended.\n```\n\n```suspense-story\ntitle: Archive closed sprints\nmode: chain\ntarget: new\nspec:\n- No match: archiving a closed sprint\nprompt:\nA closed sprint is archived with its stories. The spec has nothing on\narchiving sprints yet, so this is new.\n```"
    )
}

/// How a proposal is grounded in the spec, as planning.pi says: read from
/// the compiled reference at `reference`, cited into the Piton source under
/// `spec`.
fn grounding(reference: &str, spec: &str) -> String {
    use crate::baked_prompts::{fill, planning};
    fill(
        planning::GROUNDING,
        &[("reference", reference), ("spec", spec)],
    )
}

/// The request asking the harness to propose stories for `wanted`, shown
/// the `stories` it may add to, by id and title, or, asked `about` a story,
/// that story, its title, prompt, mode, and steps, and none to add to, since
/// a draft about a story proposes only new stories.
pub fn draft_request(
    wanted: &str,
    stories: &[(String, String)],
    about: Option<&Story>,
    reference: &str,
    spec: &str,
) -> String {
    // Written in system-prompts/planning.pi.
    use crate::baked_prompts::planning;
    let mut request = format!(
        "{}\n\n{}\n\n",
        planning::REQUEST,
        grounding(reference, spec)
    );
    if about.is_none() && !stories.is_empty() {
        request.push_str(&format!("{}:\n", planning::STORIES));
        for (id, title) in stories {
            request.push_str(&format!("- {id}: {title}\n"));
        }
        request.push('\n');
    }
    if let Some(story) = about {
        request.push_str(&format!(
            "{}: {}\nTitle: {}\nMode: {}\nPrompt:\n{}\n",
            planning::ABOUT,
            story.name,
            story.title,
            mode_slug(story.mode()),
            story.text()
        ));
        for (ix, step) in story.steps().iter().enumerate() {
            request.push_str(&format!("Step {}: {}\n", ix + 2, step.prompt));
        }
        request.push_str(&format!("\n{}\n\n", planning::SPLIT));
    }
    request.push_str(&format!("{}\n\n{}\n\n", planning::REPLY, story_example(spec)));
    if about.is_some() {
        request.push_str(&format!(
            "{}\n\n{}\n\n",
            planning::SPLIT_EXAMPLE,
            split_example(spec)
        ));
    }
    request.push_str(&format!("{}:\n{wanted}", planning::WANTED));
    request
}

/// The request asking the harness to research `topic`, as `research.pi`
/// says, shown the `stories` a proposal may add to, and the story it was
/// asked `about`, where there is one.
pub fn research_request(
    topic: &str,
    stories: &[(String, String)],
    about: Option<&Story>,
    reference: &str,
    spec: &str,
) -> String {
    // Written in system-prompts/research.pi.
    use crate::baked_prompts::research;
    let mut request = format!("{}\n\n", research::REQUEST);
    if !stories.is_empty() {
        request.push_str(&format!("{}:\n", research::STORIES));
        for (id, title) in stories {
            request.push_str(&format!("- {id}: {title}\n"));
        }
        request.push('\n');
    }
    if let Some(story) = about {
        request.push_str(&format!(
            "{}: {}\nTitle: {}\nPrompt:\n{}\n\n",
            research::ABOUT,
            story.name,
            story.title,
            story.text()
        ));
    }
    // Its proposals grounded as drafting's are.
    request.push_str(&format!(
        "{}\n\n{}\n\n{}\n\n{}:\n{topic}",
        research::REPLY,
        story_example(spec),
        grounding(reference, spec),
        research::TOPIC
    ));
    request
}

/// What a line the harness streams says it is doing, as the drafting
/// panel's live line reads it, given the tool it last started.
fn activity(line: &str, tool: &mut String) -> Option<String> {
    let event: serde_json::Value = serde_json::from_str(line).ok()?;
    let mut said = None;
    for event in crate::harness::parse(&event) {
        match event {
            crate::harness::HarnessEvent::ToolStarted { name, .. } => *tool = name,
            crate::harness::HarnessEvent::ToolInput { summary, .. } => {
                let doing = match tool.as_str() {
                    "Read" => "Reading",
                    "Grep" => "Searching for",
                    "Glob" => "Looking for",
                    other => other,
                };
                said = Some(format!("{doing} {summary}…"));
            }
            crate::harness::HarnessEvent::TextStarted => {
                said = Some("Writing its proposals…".into());
            }
            _ => {}
        }
    }
    said
}

// --- The view -----------------------------------------------------------------

/// The story open beneath its row, and its editors.
struct OpenStory {
    name: String,
    title: Entity<InputState>,
    prompt: Entity<EditorState>,
    completion: Entity<CompletionMenu>,
    fit: GrowToFit,
    /// The step the agent added, highlighted this once.
    highlight: Option<usize>,
    /// Add citation: the spec's elements searched by name, to add one.
    spec_input: Entity<InputState>,
    /// The spec's elements, from its Piton source, as links and names.
    spec_elements: Rc<Vec<(String, String)>>,
    /// Why the citation given couldn't be added.
    spec_error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// A story being written in its list, as the PlanningScope's creating
/// says: not yet a story, only a card with its title box.
struct Composer {
    /// The Backlog or Next sprint.
    section: Section,
    /// Where in the list its story will sit.
    at: usize,
    title: Entity<InputState>,
    mode: SendMode,
    /// What opened it, the "Add story" line or the heading's "+ Add", for
    /// Escape to give the focus back to.
    opener: FocusHandle,
    /// The list is yet to scroll to show it.
    reveal: bool,
    _subscriptions: Vec<Subscription>,
}

/// How many of the spec's elements Add citation lists at once.
const CITATION_PICKS: usize = 8;

/// The spec's elements whose names hold every word of `query`, ignoring
/// case, those it begins first, as Add citation lists them.
fn citation_picks<'a>(elements: &'a [(String, String)], query: &str) -> Vec<&'a (String, String)> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let mut picks: Vec<&(String, String)> = elements
        .iter()
        .filter(|(_, name)| {
            let name = name.to_lowercase();
            query.split_whitespace().all(|word| name.contains(word))
        })
        .collect();
    picks.sort_by_key(|(_, name)| !name.to_lowercase().starts_with(&query));
    picks.truncate(CITATION_PICKS);
    picks
}

/// A step of the open story opened in place to edit.
struct StepEditor {
    story: String,
    at: usize,
    prompt: Entity<TextareaState>,
    _subscription: Subscription,
}

/// A step's row dragged among its story's steps.
#[derive(Clone)]
struct StepDrag {
    story: String,
    from: usize,
    label: SharedString,
}

impl Render for StepDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_3()
            .py_1()
            .rounded(theme.radius)
            .bg(crate::theme::color(crate::theme::palette(cx).raised))
            .shadow_md()
            .text_sm()
            .child(self.label.clone())
    }
}

/// The drafting panel, as the PlanningScope's drafting says: the request,
/// the run while it goes, and how it ended.
struct DraftPanel {
    input: Entity<TextareaState>,
    /// The story it was opened from, beneath which it shows.
    about: Option<String>,
    /// While the run goes: when it started.
    running: Option<Instant>,
    /// What the run is doing, as it last said.
    activity: Arc<Mutex<String>>,
    cancel: Arc<Cancel>,
    /// A run that failed: what was reported.
    error: Option<String>,
    details_open: bool,
    /// The last run proposed nothing.
    proposed_nothing: bool,
    /// It researches a topic rather than drafting stories.
    research: bool,
    _run: Option<Task<()>>,
    /// Shows the request as it is typed, so Draft is enabled once it holds
    /// something.
    _typed: Subscription,
}

/// A proposal being edited in its card.
struct ProposalEditor {
    id: String,
    title: Entity<InputState>,
    prompt: Entity<TextareaState>,
    _subscriptions: Vec<Subscription>,
}

/// A proposal rejected, which can be brought back for a few seconds.
struct Rejected {
    proposal: Proposal,
    /// The story whose own draft it waited in; none for the drafting
    /// panel's.
    owner: Option<String>,
    at: usize,
    when: Instant,
}

/// A decided proposal's note: what it says, and the story it links to.
struct Decided {
    text: String,
    story: String,
    /// The story whose own draft it waited in; none for the drafting
    /// panel's.
    owner: Option<String>,
}

/// A story's own draft with the agent, as the PlanningScope's drafting with
/// the agent says: its run while it goes, and how the last one ended. What
/// it proposed waits in the plan's notes, saved with it.
#[derive(Default)]
struct StoryDraft {
    /// While its run goes: when it started.
    running: Option<Instant>,
    cancel: Arc<Cancel>,
    /// What the run has written so far, from which Stop keeps what it had
    /// proposed by then.
    written: Arc<Mutex<String>>,
    /// A run that failed: what was reported.
    error: Option<String>,
    details_open: bool,
    /// The last run proposed nothing.
    proposed_nothing: bool,
    _run: Option<Task<()>>,
}

/// A story's row dragged.
#[derive(Clone)]
struct StoryDrag {
    name: String,
    /// The other selected stories it carries, in the selection's order,
    /// while it is one of them.
    carried: Vec<String>,
    /// What its copy shows, and how big it is.
    face: CardFace,
    size: Size<Pixels>,
    /// Where on the card it was grabbed, set as it is picked up.
    grab: Rc<Cell<Point<Pixels>>>,
}

impl Render for StoryDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cx: &App = cx;
        let palette = crate::theme::palette(cx);
        // The cards it carries, stacked a few pixels behind it.
        let stack = (1..=self.carried.len().min(2)).rev().map(|depth| {
            let by = px(4. * depth as f32);
            div()
                .absolute()
                .left(by)
                .top(by)
                .w(self.size.width)
                .h(self.size.height)
                .rounded(px(6.))
                .border_1()
                .border_color(crate::theme::color(palette.edge))
                .bg(crate::theme::color(palette.overlay))
        });
        div()
            .relative()
            .children(stack)
            .child(crate::animations::tilt::tilted(self.size, TILT, || {
                card_copy(&self.face, 1., cx).into_any_element()
            }))
    }
}

/// What a story card shows, for a copy of it to be drawn from.
#[derive(Clone)]
struct CardFace {
    title: SharedString,
    untitled: bool,
    mode: SendMode,
    from_agent: bool,
    steps: usize,
    attached: bool,
    cited: bool,
    updated: bool,
    /// How many cards are dragged with it, itself among them, beside its
    /// title while it carries others.
    count: usize,
}

/// How far a picked-up card is turned, in degrees.
const TILT: f32 = 3.;

/// A full-size copy of a story card, as the parts draw one, on the overlay
/// surface with its edge border, floating: its shadow as strong as `shadow`,
/// from 0 for none to 1 for the theme's elevation. It takes no clicks.
fn card_copy(face: &CardFace, shadow: f32, cx: &App) -> Div {
    let palette = crate::theme::palette(cx);
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    // Soft, short, and black, stronger in dark mode than in light.
    let strength = if theme.is_dark() { 0.5 } else { 0.18 };
    v_flex()
        .size_full()
        .py_2()
        .px(px(10.))
        .rounded(px(6.))
        .border_1()
        .border_color(crate::theme::color(palette.edge))
        .bg(crate::theme::color(palette.overlay))
        .when(shadow > 0., |card| {
            card.shadow(vec![BoxShadow {
                color: hsla(0., 0., 0., strength * shadow),
                offset: point(px(0.), px(4.)),
                blur_radius: px(12.),
                spread_radius: px(0.),
                inset: false,
            }])
        })
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_start()
                .child(
                    div()
                        .w(GRIP_WIDTH)
                        .h(px(20.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(IconName::GripVertical)
                                .xsmall()
                                .text_color(crate::theme::color(palette.text_tertiary)),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .line_clamp(2)
                        .text_ellipsis()
                        .text_color(if face.untitled { muted } else { theme.foreground })
                        .child(face.title.clone()),
                )
                .when(face.count > 1, |line| {
                    line.child(
                        div()
                            .flex_none()
                            .px_1()
                            .rounded(px(4.))
                            .bg(theme.primary)
                            .text_xs()
                            .text_color(theme.primary_foreground)
                            .child(face.count.to_string()),
                    )
                }),
        )
        .child(
            h_flex()
                .w_full()
                .mt_1()
                .h(PROPERTY_HEIGHT)
                .gap_2()
                .items_center()
                .text_xs()
                .text_color(muted)
                .child(div().flex_none().px_1().child(mode_pill_label(face.mode, cx)))
                .when(face.from_agent, |meta| {
                    meta.child(Icon::new(IconName::Sparkles).xsmall().text_color(theme.primary))
                })
                .when(face.steps > 0, |meta| {
                    meta.child(div().flex_none().child(format!("{} steps", face.steps + 1)))
                })
                .when(face.attached, |meta| {
                    meta.child(Icon::new(IconName::Paperclip).xsmall().text_color(muted))
                })
                .when(face.cited, |meta| {
                    meta.child(Icon::new(IconName::BookOpen).xsmall().text_color(muted))
                })
                .when(face.updated, |meta| {
                    meta.child(
                        div()
                            .flex_none()
                            .text_color(theme.primary)
                            .child("Updated by agent"),
                    )
                }),
        )
}

/// A card picked up and dragged, as the PlanningScope's motion says.
struct Lift {
    name: String,
    /// The other selected stories it carries, in order.
    carried: Vec<String>,
    /// The gap it left: its column, and its place there.
    from: (Section, usize),
    size: Size<Pixels>,
    /// How tall the gap it opens is: the cards it carries together.
    height: Pixels,
    /// Where on the card it was grabbed.
    grab: Point<Pixels>,
}

/// A placeholder the dragged card's height, opening or closing in a column.
struct Slot {
    /// Its column, and the place among its cards it opens before.
    at: (Section, usize),
    height: Pixels,
    open: crate::animations::slide::Toward,
}

/// A dropped card's copy settling into its place, straightening as its
/// shadow fades.
struct Settle {
    name: String,
    face: CardFace,
    size: Size<Pixels>,
    /// Where the copy was let go.
    from: Point<Pixels>,
    since: Instant,
}

/// How an opened story's card is kept in view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reveal {
    /// Kept fully in view as the board narrows, when it was before.
    Keep,
    /// Scrolled to, sideways and up or down, until it is fully in view.
    Show,
}

/// The board or a column scrolling, in the quick motion, `by` along `axis`,
/// so a card comes fully into view; how much it has moved so far, so
/// anything else that scrolls it meanwhile is kept.
struct Glide {
    handle: ScrollHandle,
    axis: Axis,
    by: Pixels,
    done: Pixels,
    since: Instant,
}

pub struct Planning {
    project_dir: PathBuf,
    /// Next sprint, Backlog, and Done, in order.
    sections: [Vec<Story>; 5],
    /// The proposals waiting to be decided, in the order given.
    proposals: Vec<Proposal>,
    /// The research done, and whether planning is paused for review.
    notes: Notes,
    open: Option<OpenStory>,
    /// The one story being written in its list.
    composer: Option<Composer>,
    /// Each list's heading's "+ Add", by its key.
    add_top_focus: std::collections::HashMap<&'static str, FocusHandle>,
    /// Each list's heading, to tell whether the focus is in it.
    heading_focus: std::collections::HashMap<&'static str, FocusHandle>,
    drafting: Option<DraftPanel>,
    /// Each story's own draft with the agent, by its name.
    story_drafts: std::collections::HashMap<String, StoryDraft>,
    /// The open story was rewritten: its editors take its new title and
    /// prompt next frame.
    refresh_open: bool,
    editing: Option<ProposalEditor>,
    /// The step of the open story being edited.
    open_step: Option<StepEditor>,
    /// A step that couldn't be added: to which story, why, and what it
    /// held, kept so it can be added elsewhere.
    step_error: Option<(String, String, String)>,
    /// Each step row's focus, by its story and place.
    step_focus: std::collections::HashMap<(String, usize), FocusHandle>,
    /// While a step is dragged: the gap it would land in.
    step_gap: Rc<Cell<Option<usize>>>,
    /// Proposals whose prompts are shown whole.
    read_more: std::collections::HashSet<String>,
    /// The lists of steps, in Done and in findings, shown whole, by where
    /// they are listed.
    steps_expanded: std::collections::HashSet<String>,
    rejected: Vec<Rejected>,
    decided: Vec<Decided>,
    /// Why a proposal couldn't be accepted, by its id.
    refused: std::collections::HashMap<String, String>,
    /// The mode picker open: the story's, and the mode highlighted.
    mode_menu: Option<(String, usize)>,
    /// The story asking to be deleted.
    confirm_delete: Option<String>,
    /// The stories selected, to be acted on together, as the
    /// PlanningScope's parts say: only ever in the Backlog and Next sprint,
    /// and never saved with the plan.
    selected: std::collections::HashSet<String>,
    /// Each list's card last clicked or toggled, by its key, where a
    /// Shift+click's range starts.
    select_from: std::collections::HashMap<&'static str, String>,
    /// The card under the pointer, which the single-story shortcuts act on
    /// while no card has focus.
    hovered: Option<String>,
    /// The bulk bar's mode picker, while open: the mode highlighted.
    bulk_mode_menu: Option<usize>,
    /// The bulk bar asking to delete every selected story.
    confirm_bulk_delete: bool,
    /// The bulk bar, sliding up or away.
    bulk_shown: crate::animations::slide::Toward,
    /// The bulk bar's focus, a stop of its own for F6.
    bulk_focus: FocusHandle,
    hooks: PlanningHooks,
    focus: FocusHandle,
    /// Each row's and proposal's focus, by its story's name or its id.
    row_focus: std::collections::HashMap<String, FocusHandle>,
    /// The board's sideways scroll.
    board_scroll: ScrollHandle,
    /// The side panel's own scroll.
    panel_scroll: ScrollHandle,
    /// Each column's own scroll, by its key.
    column_scroll: std::collections::HashMap<&'static str, ScrollHandle>,
    /// Each column's cards, held still as what is above the view changes,
    /// by its key.
    steady: std::collections::HashMap<&'static str, crate::animations::steady::Steady>,
    /// How tall each column's cards were laid out, by its key, so only the
    /// cards in view are laid out each frame.
    column_cards: std::collections::HashMap<&'static str, Rc<std::cell::RefCell<ColumnCards>>>,
    /// The open story's card, to be kept or brought into view.
    reveal: Option<(String, Reveal)>,
    /// The board and columns scrolling to show the open story's card.
    glides: Vec<Glide>,
    /// Where each card and row was last drawn, by the name its focus is
    /// kept under, so Left and Right land on the one nearest in height.
    row_bounds: Rc<std::cell::RefCell<std::collections::HashMap<String, Bounds<Pixels>>>>,
    /// While a row is dragged: the gap it would land in.
    gap: Rc<Cell<Option<(Section, usize)>>>,
    /// While a row is dragged: its story's name, and where the pointer is.
    dragged: Rc<std::cell::RefCell<Option<(String, Point<Pixels>)>>>,
    /// While a selected row is dragged: the other selected stories it
    /// carries.
    drag_carried: Rc<std::cell::RefCell<Vec<String>>>,
    /// Where each column was last drawn, by its key: the whole column, and
    /// the part its cards scroll in.
    column_bounds: Rc<std::cell::RefCell<ColumnBounds>>,
    /// Where the board was last drawn.
    board_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// While a wheel turn or swipe over the board is handed on: where the
    /// board and each column were scrolled to before it, so it moves them
    /// only as [`Self::wheel`] says.
    wheel_from: Rc<std::cell::RefCell<Option<WheelFrom>>>,
    /// While a dragged row is held near an edge, the board or its column
    /// is scrolling on, frame by frame.
    drag_scrolling: bool,
    /// The card picked up, while it is dragged.
    lift: Option<Lift>,
    /// The placeholders open, opening, or closing while a card is dragged.
    slots: Vec<Slot>,
    /// Where the open placeholder was last drawn.
    slot_at: Rc<Cell<Option<Point<Pixels>>>>,
    /// A dropped card settling into its place.
    settle: Option<Settle>,
    /// The cards, sliding to their new places.
    slides: crate::animations::slide::Slides,
    /// The plan changed: what moved slides to its new place next frame.
    slide_next: Cell<bool>,
    /// The side panel, sliding in or out.
    panel_shown: crate::animations::slide::Toward,
    /// Whether Planning's tab is the one shown in the Body.
    shown: bool,
    /// Its tab was selected again: the next frame shows the board as it
    /// was left, with nothing moving, as the PlanningScope's layout says.
    returning: bool,
    /// The plan's files waiting to be written, each with what is latest
    /// for it, none to remove it.
    writes: Arc<Mutex<std::collections::HashMap<PathBuf, Option<String>>>>,
    /// Ticks once a second while something runs, for its time.
    _tick: Option<Task<()>>,
    /// The sprint is being started by itself now.
    advancing: bool,
    /// Next sprint, found waiting as the plan was read with Start
    /// automatically on, held until Start now: a sprint never begins unseen
    /// as the project opens.
    waiting_start: bool,
    /// How many empty stories the last Send sprint left in Next sprint, as
    /// it says beside its heading while they are still there.
    left_empty: usize,
    /// The plan has been read from the project.
    loaded: bool,
    /// Why the plan couldn't be read, naming the file.
    load_error: Option<String>,
    /// A change made before the plan was read, to save once it is.
    save_waiting: Cell<bool>,
    /// Why a file of the plan couldn't be saved, naming it.
    save_error: Arc<Mutex<Option<String>>>,
    /// The pause bar's focus, a stop of its own for F6.
    pause_focus: FocusHandle,
    /// An open panel's focus, a stop of its own for F6.
    panel_focus: FocusHandle,
    /// Each section's focus, by its key, a stop for F6.
    section_focus: std::collections::HashMap<&'static str, FocusHandle>,
    /// The Start automatically switch's focus, reached by Tab.
    switch_focus: FocusHandle,
    /// The region F6 last moved to.
    region: usize,
}

impl EventEmitter<()> for Planning {}

impl Focusable for Planning {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Planning {
    pub fn new(project_dir: PathBuf, hooks: PlanningHooks, cx: &mut Context<Self>) -> Self {
        // Read in the background; skeleton rows meanwhile.
        let dir = project_dir.clone();
        cx.spawn(async move |this, cx| {
            let read = cx.background_spawn(async move { load_checked(&dir) }).await;
            this.update(cx, |this, cx| this.read(read, cx)).ok();
        })
        .detach();
        let section_focus = ["draft", "current", "next", "backlog", "findings", "done"]
            .into_iter()
            .map(|key| (key, cx.focus_handle()))
            .collect();
        let column_scroll = ["draft", "current", "next", "backlog", "findings", "done"]
            .into_iter()
            .map(|key| (key, ScrollHandle::new()))
            .collect();
        let this = Self {
            project_dir,
            sections: Default::default(),
            proposals: Vec::new(),
            notes: Notes::default(),
            open: None,
            composer: None,
            add_top_focus: LISTS
                .into_iter()
                .map(|section| (section.key(), cx.focus_handle().tab_stop(true)))
                .collect(),
            heading_focus: LISTS
                .into_iter()
                .map(|section| (section.key(), cx.focus_handle()))
                .collect(),
            drafting: None,
            story_drafts: Default::default(),
            refresh_open: false,
            editing: None,
            open_step: None,
            step_error: None,
            step_focus: Default::default(),
            step_gap: Rc::default(),
            read_more: Default::default(),
            steps_expanded: Default::default(),
            rejected: Vec::new(),
            decided: Vec::new(),
            refused: Default::default(),
            mode_menu: None,
            confirm_delete: None,
            selected: Default::default(),
            select_from: Default::default(),
            hovered: None,
            bulk_mode_menu: None,
            confirm_bulk_delete: false,
            bulk_shown: crate::animations::slide::Toward::at(false),
            bulk_focus: cx.focus_handle(),
            hooks,
            focus: cx.focus_handle(),
            row_focus: Default::default(),
            board_scroll: ScrollHandle::new(),
            column_scroll,
            steady: ["draft", "current", "next", "backlog", "findings", "done"]
                .into_iter()
                .map(|key| (key, Default::default()))
                .collect(),
            column_cards: ["draft", "current", "next", "backlog", "findings", "done"]
                .into_iter()
                .map(|key| (key, Default::default()))
                .collect(),
            reveal: None,
            glides: Vec::new(),
            row_bounds: Rc::default(),
            gap: Rc::default(),
            dragged: Rc::default(),
            drag_carried: Rc::default(),
            column_bounds: Rc::default(),
            board_bounds: Rc::default(),
            wheel_from: Rc::default(),
            drag_scrolling: false,
            lift: None,
            slots: Vec::new(),
            slot_at: Rc::default(),
            settle: None,
            slides: Default::default(),
            slide_next: Cell::new(false),
            panel_shown: crate::animations::slide::Toward::at(false),
            shown: false,
            returning: false,
            writes: Arc::default(),
            _tick: None,
            advancing: false,
            waiting_start: false,
            left_empty: 0,
            loaded: false,
            load_error: None,
            save_waiting: Cell::new(false),
            save_error: Arc::default(),
            pause_focus: cx.focus_handle().tab_stop(true),
            panel_focus: cx.focus_handle(),
            panel_scroll: ScrollHandle::new(),
            section_focus,
            switch_focus: cx.focus_handle().tab_stop(true),
            region: 0,
        };
        this.publish(cx);
        this
    }

    /// The plan as read from the project: what was added meanwhile is kept
    /// after what was saved, and saved once it is in. Where it couldn't be
    /// read, the list says so, naming the file, and nothing is saved over
    /// it.
    fn read(&mut self, read: Result<Plan, String>, cx: &mut Context<Self>) {
        match read {
            Ok((mut sections, mut proposals, notes)) => {
                for (saved, added) in sections.iter_mut().zip(self.sections.iter_mut()) {
                    saved.append(added);
                }
                proposals.append(&mut self.proposals);
                let mut notes = notes;
                notes.findings.append(&mut self.notes.findings);
                notes.paused = notes.paused.or(self.notes.paused.take());
                notes.auto_start |= self.notes.auto_start;
                for (name, added) in std::mem::take(&mut self.notes.drafts) {
                    let saved = notes.drafts.entry(name).or_default();
                    saved.proposals.extend(added.proposals);
                    saved.running |= added.running;
                }
                // A run going as the application quit was cut off.
                for (name, saved) in &notes.drafts {
                    let draft = self.story_drafts.entry(name.clone()).or_default();
                    if saved.running && draft.running.is_none() {
                        draft.error = Some(CUT_OFF.into());
                    }
                }
                self.sections = sections;
                self.proposals = proposals;
                self.notes = notes;
                self.loaded = true;
                self.load_error = None;
                self.waiting_start = self.notes.auto_start
                    && self.sections[section_ix(Section::Current)].is_empty()
                    && self.has_sendable();
                if self.save_waiting.take() {
                    self.save_order(cx);
                }
                self.publish(cx);
            }
            Err(why) => self.load_error = Some(why),
        }
        cx.notify();
    }

    /// F6: focus moves to the next of the column's regions, the pause bar,
    /// each section, and an open panel, each a single stop: a section's
    /// first row, or the section itself while it holds none.
    fn next_region(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut regions: Vec<FocusHandle> = Vec::new();
        if self.notes.paused.is_some() {
            regions.push(self.pause_focus.clone());
        }
        // Each column, in the board's order, at its first card.
        for section in SECTIONS {
            let first = self.sections[section_ix(section)]
                .first()
                .map(|story| story.name.clone());
            match first {
                Some(name) => regions.push(self.row_focus(&name, cx)),
                None => regions.push(self.section_focus[section.key()].clone()),
            }
        }
        let first_finding = self
            .notes
            .findings
            .last()
            .map(|finding| finding.file.clone());
        regions.push(match first_finding {
            Some(file) => self.row_focus(&format!("finding:{file}"), cx),
            None => self.section_focus["findings"].clone(),
        });
        if !self.selected.is_empty() {
            regions.push(self.bulk_focus.clone());
        }
        if let Some(open) = &self.open {
            regions.push(open.title.focus_handle(cx));
        } else if let Some(drafting) = &self.drafting {
            regions.push(drafting.input.focus_handle(cx));
        }
        // On from the region holding the focus, or the one last moved to.
        let at = regions
            .iter()
            .position(|region| region.contains_focused(window, cx))
            .unwrap_or(self.region);
        self.region = (at + 1) % regions.len();
        regions[self.region].focus(window, cx);
        cx.notify();
    }

    /// Reads the plan again, as its error's Try again does.
    fn read_again(&mut self, cx: &mut Context<Self>) {
        self.load_error = None;
        let dir = self.project_dir.clone();
        cx.spawn(async move |this, cx| {
            let read = cx.background_spawn(async move { load_checked(&dir) }).await;
            this.update(cx, |this, cx| this.read(read, cx)).ok();
        })
        .detach();
        cx.notify();
    }

    /// The stories of `section`.
    #[cfg(test)]
    pub fn stories(&self, section: Section) -> &[Story] {
        &self.sections[section_ix(section)]
    }

    /// Where the sprint stands, for the ribbon.
    pub fn pending(&self) -> SprintPending {
        SprintPending {
            waiting: self.unsent(),
            under_way: !self.sections[3].is_empty(),
            paused: self.notes.paused.is_some(),
            auto_start: self.notes.auto_start,
        }
    }

    /// Whether planning is paused for review.
    pub fn is_paused(&self) -> bool {
        self.notes.paused.is_some()
    }

    /// While planning is paused for review, the stories of Current sprint,
    /// by the names they were sent as: what is held until it ends.
    pub fn held_for_review(&self) -> Option<std::collections::HashSet<String>> {
        self.notes.paused.as_ref()?;
        Some(
            self.sections[section_ix(Section::Current)]
                .iter()
                .filter_map(|story| story.task.clone())
                .collect(),
        )
    }

    /// The names every story sent was sent as.
    pub fn story_tasks(&self) -> std::collections::HashSet<String> {
        self.sections
            .iter()
            .flatten()
            .filter_map(|story| story.task.clone())
            .collect()
    }

    /// How many of Current sprint's stories are running, or running a step.
    fn running(&self, cx: &App) -> usize {
        self.sections[section_ix(Section::Current)]
            .iter()
            .filter(|story| {
                let Some(task) = story.task.as_deref() else {
                    return false;
                };
                self.state(story, cx) == Some(StoryState::Running)
                    || (0..story.steps().len()).any(|at| {
                        (self.hooks.step_state)(task, at, cx)
                            .is_some_and(|(state, _)| state == StoryState::Running)
                    })
            })
            .count()
    }

    /// A story's status as its row shows it: "Held for review", muted, for
    /// one of Current sprint still waiting while planning is paused.
    fn status_shown(
        &self,
        status: Option<(StoryState, Option<Duration>)>,
        current: bool,
        cx: &App,
    ) -> Option<AnyElement> {
        let (state, elapsed) = status?;
        if current && state == StoryState::Queued && self.is_paused() {
            return Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Held for review")
                    .into_any_element(),
            );
        }
        Some(status_label(state, elapsed, cx))
    }

    /// Says how many stories wait in Next sprint to be sent, for the ribbon.
    fn publish(&self, cx: &mut App) {
        cx.set_global(self.pending());
    }

    fn unsent(&self) -> usize {
        self.sections[0]
            .iter()
            .filter(|story| story.task.is_none())
            .count()
    }

    fn find(&self, name: &str) -> Option<(Section, usize)> {
        SECTIONS.into_iter().find_map(|section| {
            self.sections[section_ix(section)]
                .iter()
                .position(|story| story.name == name)
                .map(|ix| (section, ix))
        })
    }

    fn story(&self, name: &str) -> Option<&Story> {
        let (section, ix) = self.find(name)?;
        self.sections[section_ix(section)].get(ix)
    }

    fn story_mut(&mut self, name: &str) -> Option<&mut Story> {
        let (section, ix) = self.find(name)?;
        self.sections[section_ix(section)].get_mut(ix)
    }

    /// The title of the story named so, while it exists.
    pub fn title_of(&self, name: &str) -> Option<String> {
        self.story(name).map(|story| story.title.clone())
    }

    /// The proposal `id`, in the drafting panel or waiting for a story.
    fn proposal(&self, id: &str) -> Option<&Proposal> {
        self.proposals
            .iter()
            .chain(self.notes.drafts.values().flat_map(|draft| &draft.proposals))
            .find(|proposal| proposal.id == id)
    }

    fn proposal_mut(&mut self, id: &str) -> Option<&mut Proposal> {
        self.proposals
            .iter_mut()
            .chain(
                self.notes
                    .drafts
                    .values_mut()
                    .flat_map(|draft| &mut draft.proposals),
            )
            .find(|proposal| proposal.id == id)
    }

    /// The story whose own draft the proposal `id` waits in, if it does.
    fn owner_of(&self, id: &str) -> Option<String> {
        self.notes
            .drafts
            .iter()
            .find(|(_, draft)| draft.proposals.iter().any(|proposal| proposal.id == id))
            .map(|(name, _)| name.clone())
    }

    /// Takes the proposal `id` out of wherever it waits, unsaved: it, the
    /// story whose draft it waited in, and its place there.
    fn take_out(&mut self, id: &str) -> Option<(Proposal, Option<String>, usize)> {
        if let Some(at) = self.proposals.iter().position(|proposal| proposal.id == id) {
            return Some((self.proposals.remove(at), None, at));
        }
        self.notes.drafts.iter_mut().find_map(|(name, draft)| {
            let at = draft.proposals.iter().position(|proposal| proposal.id == id)?;
            Some((draft.proposals.remove(at), Some(name.clone()), at))
        })
    }

    /// Saves the proposal `id` wherever it waits.
    fn save_proposal(&self, id: &str, cx: &mut Context<Self>) {
        match self.owner_of(id) {
            Some(owner) => self.save_draft(&owner, cx),
            None => self.save_order(cx),
        }
    }

    /// Whether a proposal of the drafting panel's waits on its Undo.
    fn panel_rejected(&self) -> bool {
        self.rejected.iter().any(|rejected| rejected.owner.is_none())
    }

    /// Saves the order and the proposals in the background.
    fn save_order(&self, cx: &mut Context<Self>) {
        // Every move is saved, and slides to its place.
        self.slide_next.set(true);
        if !self.loaded {
            // Never over a plan not read yet.
            self.save_waiting.set(true);
            self.publish(cx);
            return;
        }
        let text = order_of(&self.sections, &self.proposals, &self.notes);
        let file = planning_dir(&self.project_dir).join(ORDER_FILE);
        self.write_later(file, Some(text), cx);
        self.publish(cx);
        if self.notes.auto_start && !self.advancing {
            let this = cx.entity().downgrade();
            cx.defer(move |cx| {
                this.update(cx, |this, cx| this.auto_advance(cx)).ok();
            });
        }
    }

    /// Saves the story named so in the background.
    fn save_story_named(&self, name: &str, cx: &mut Context<Self>) {
        let Some(story) = self.story(name) else {
            return;
        };
        self.write_later(story_file(&self.project_dir, name), Some(story.source()), cx);
    }

    /// Writes `text` to `file` in the background, or with none removes it.
    /// Whichever write runs takes what is latest for its file, so writes
    /// made one after another never land out of order, however the
    /// background runs them.
    fn write_later(&self, file: PathBuf, text: Option<String>, cx: &mut Context<Self>) {
        let writes = self.writes.clone();
        writes.lock().unwrap().insert(file.clone(), text);
        let failed = self.save_error.clone();
        let this = cx.entity().downgrade();
        let write = cx.background_spawn(async move {
            let mut pending = writes.lock().unwrap();
            let Some(text) = pending.remove(&file) else {
                // A later write took it, with what is newer.
                return false;
            };
            let written = match text {
                Some(text) => file
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|_| std::fs::write(&file, text)),
                None => match std::fs::remove_file(&file) {
                    Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
                    _ => Ok(()),
                },
            };
            drop(pending);
            match written {
                Err(err) => {
                    let name = file.file_name().map_or_else(
                        || file.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                    *failed.lock().unwrap() = Some(format!("{name} couldn't be saved: {err}"));
                    true
                }
                Ok(()) => false,
            }
        });
        cx.spawn(async move |_, cx| {
            if write.await {
                this.update(cx, |_, cx| cx.notify()).ok();
            }
        })
        .detach();
    }

    /// The ribbon's or the palette's New story: the composer at the foot of
    /// Next sprint, as Ctrl/Cmd+N opens it with no list focused.
    pub fn new_story(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.compose_at_foot(Section::Next, window, cx);
    }

    /// The composer at the foot of `section`, in its "Add story" line's
    /// place, the board scrolled sideways to show it.
    fn compose_at_foot(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        let opener = self.row_focus(&add_line(section), cx);
        let at = self.sections[section_ix(section)].len();
        self.open_composer(section, at, opener, window, cx);
        if let Some(column) = SECTIONS.iter().position(|shown| *shown == section) {
            self.board_scroll.scroll_to_item(column);
        }
    }

    /// The composer at the top of `section`, from its heading's "+ Add".
    fn compose_at_top(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        let opener = self.add_top_focus[section.key()].clone();
        self.open_composer(section, 0, opener, window, cx);
    }

    /// Opens the composer at place `at` of `section`, its title focused, in
    /// the mode last picked. Only one is open at a time: the one open
    /// before is settled as clicking away settles it.
    fn open_composer(
        &mut self,
        section: Section,
        at: usize,
        opener: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(section, Section::Draft | Section::Backlog | Section::Next) {
            return;
        }
        self.settle_composer(cx);
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Story title"));
        let subscription = cx.subscribe_in(
            &title,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.composer_entered(window, cx),
                // Clicking away: an empty one is discarded, one with a title
                // added, with no other opened. Another window coming to the
                // front leaves it as it is.
                InputEvent::Blur => {
                    let ours = this
                        .composer
                        .as_ref()
                        .is_some_and(|composer| composer.title == *input);
                    let focused = input.read(cx).focus_handle(cx).is_focused(window);
                    if ours && !focused && window.is_window_active() {
                        this.settle_composer(cx);
                    }
                }
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        );
        title.update(cx, |input, cx| input.focus(window, cx));
        self.composer = Some(Composer {
            section,
            at: at.min(self.sections[section_ix(section)].len()),
            title,
            mode: last_mode(),
            opener,
            reveal: true,
            _subscriptions: vec![subscription],
        });
        cx.notify();
    }

    /// Closes the composer, adding its story where it was when it has a
    /// title: where the story was added, if it was.
    fn settle_composer(&mut self, cx: &mut Context<Self>) -> Option<(Section, usize)> {
        let composer = self.composer.take()?;
        cx.notify();
        let title = composer.title.read(cx).value().trim().to_string();
        if title.is_empty() {
            return None;
        }
        Some(self.insert_story(composer.section, composer.at, title, composer.mode, cx))
    }

    /// Enter in the composer: its story is added, never opened, and another
    /// composer opens beneath it, so a list can be written down without the
    /// mouse. With its title empty, nothing is added.
    fn composer_entered(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(composer) = &self.composer else {
            return;
        };
        if composer.title.read(cx).value().trim().is_empty() {
            return;
        }
        let opener = composer.opener.clone();
        if let Some((section, at)) = self.settle_composer(cx) {
            self.open_composer(section, at + 1, opener, window, cx);
        }
    }

    /// Escape in the composer: it is discarded, whatever was typed, the
    /// focus going back to what opened it.
    fn discard_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(composer) = self.composer.take() else {
            return false;
        };
        composer.opener.focus(window, cx);
        cx.notify();
        true
    }

    /// Adds a story titled so, with no prompt, at place `at` of `section`:
    /// where it went.
    fn insert_story(
        &mut self,
        section: Section,
        at: usize,
        title: String,
        mode: SendMode,
        cx: &mut Context<Self>,
    ) -> (Section, usize) {
        let story = Story::new(title, String::new(), mode, &self.project_dir);
        let name = story.name.clone();
        let stories = &mut self.sections[section_ix(section)];
        let at = at.min(stories.len());
        stories.insert(at, story);
        self.save_story_named(&name, cx);
        self.save_order(cx);
        cx.notify();
        (section, at)
    }

    /// Adds a story of `mode` holding `prompt` at the end of `section`.
    pub fn add_story(
        &mut self,
        section: Section,
        title: String,
        prompt: String,
        mode: SendMode,
        cx: &mut Context<Self>,
    ) {
        let story = Story::new(title, prompt, mode, &self.project_dir);
        let name = story.name.clone();
        self.sections[section_ix(section)].push(story);
        self.save_story_named(&name, cx);
        self.save_order(cx);
        cx.notify();
    }

    /// Opens the story named so beneath its row, closing any other. Opened,
    /// it is no longer marked as the agent's.
    fn open_story(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.as_ref().is_some_and(|open| open.name == name) {
            return;
        }
        // A story of the current sprint can't be edited there.
        if self
            .find(name)
            .is_some_and(|(section, _)| section == Section::Current)
        {
            return;
        }
        self.close_story(window, cx);
        // It replaces what the side panel held: drafting with nothing
        // running or left to decide goes; a run or proposals are kept, to
        // come back once the story closes.
        if self
            .drafting
            .as_ref()
            .is_some_and(|drafting| drafting.running.is_none())
            && self.proposals.is_empty()
            && !self.panel_rejected()
        {
            self.drafting = None;
            self.decided.retain(|decided| decided.owner.is_some());
        }
        // Opened, its card's error and "Nothing proposed" go, and a run cut
        // off is no longer said to be.
        if let Some(draft) = self.story_drafts.get_mut(name) {
            draft.error = None;
            draft.details_open = false;
            draft.proposed_nothing = false;
        }
        if !self.story_drafting(name)
            && let Some(saved) = self.notes.drafts.get_mut(name)
            && saved.running
        {
            saved.running = false;
            self.save_draft(name, cx);
        }
        let Some(story) = self.story_mut(name) else {
            return;
        };
        let marked = story.from_agent || story.updated || story.highlight.is_some();
        story.from_agent = false;
        story.updated = false;
        let highlight = story.highlight.take();
        let (title_text, prompt_text) = (story.title.clone(), story.prompt.clone());
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Story title")
                .default_value(title_text)
        });
        let prompt = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(piton_syntax::LANGUAGE_NAME)
                .line_number(false)
                .folding(false)
                .soft_wrap(true)
                .scroll_beyond_last_line(Some(0))
                .cursor_surrounding_lines(Some(0))
                .placeholder("Prompt… (the title is sent where there's none)")
                .default_value(prompt_text)
        });
        let completion = cx.new(|cx| CompletionMenu::new(prompt.clone(), window, cx));
        let provider = (self.hooks.completions)(cx);
        completion.update(cx, |menu, cx| menu.set_provider(provider, cx));
        let story_name = name.to_string();
        let subscriptions = vec![
            cx.subscribe_in(&title, window, {
                let name = story_name.clone();
                move |this, input, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => {
                        let value = input.read(cx).value().to_string();
                        if let Some(story) = this.story_mut(&name) {
                            story.title = value;
                        }
                        this.save_order(cx);
                        cx.notify();
                    }
                    _ => {}
                }
            }),
            cx.subscribe_in(&prompt, window, {
                let name = story_name.clone();
                move |this, editor, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        let value = editor.read(cx).value().to_string();
                        if let Some(story) = this.story_mut(&name) {
                            story.prompt = value;
                        }
                        this.save_story_named(&name, cx);
                        if let Some(open) = &this.open {
                            open.completion
                                .update(cx, |menu, cx| menu.on_edit(window, cx));
                        }
                    }
                    cx.notify();
                }
            }),
        ];
        let spec_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Add citation: search the spec's elements by name, or No match…")
        });
        let spec_elements = Rc::new(spec_elements(&self.project_dir));
        let mut subscriptions = subscriptions;
        subscriptions.push(cx.subscribe_in(&spec_input, window, {
            let name = story_name.clone();
            let elements = spec_elements.clone();
            move |this, input, event: &InputEvent, window, cx| match event {
                // The first element found, or what was typed, as No match.
                InputEvent::PressEnter { .. } => {
                    let text = input.read(cx).value().to_string();
                    let added = match citation_picks(&elements, &text).first() {
                        Some((link, _)) => this.add_citation(&name, link, cx),
                        None => this.add_citation(&name, &text, cx),
                    };
                    if added {
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                }
                InputEvent::Change => cx.notify(),
                _ => {}
            }
        }));
        title.update(cx, |input, cx| input.focus(window, cx));
        self.open = Some(OpenStory {
            name: story_name,
            title,
            prompt,
            completion,
            fit: GrowToFit::new(PROMPT_MAX_ROWS),
            highlight,
            spec_input,
            spec_elements,
            spec_error: None,
            _subscriptions: subscriptions,
        });
        if marked {
            self.save_order(cx);
        }
        self.reveal = Some((name.to_string(), Reveal::Keep));
        cx.notify();
    }

    /// Opens the story named so, from a click on its card or from outside
    /// the board, the board and its column scrolled just enough that its
    /// card is fully in view.
    fn show_story(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_story(name, window, cx);
        if self.open.as_ref().is_some_and(|open| open.name == name) {
            self.reveal = Some((name.to_string(), Reveal::Show));
        }
    }

    /// Closes the open story, its row focused again; one left with neither
    /// title nor prompt is removed.
    fn close_story(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_step(window, cx);
        self.step_error = None;
        let Some(open) = self.open.take() else {
            return;
        };
        self.decided
            .retain(|decided| decided.owner.as_deref() != Some(open.name.as_str()));
        let empty = self.story(&open.name).is_some_and(|story| {
            story.title.trim().is_empty()
                && story.prompt.trim().is_empty()
                && story.steps().is_empty()
        });
        if empty {
            self.delete(&open.name, cx);
            self.focus.focus(window, cx);
        } else {
            self.focus_row(&open.name, window, cx);
        }
        cx.notify();
    }

    fn toggle_story(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.as_ref().is_some_and(|open| open.name == name) {
            self.close_story(window, cx);
        } else {
            self.open_story(name, window, cx);
        }
    }

    /// Escape: closes what is open, one layer at a time: a composer, the
    /// mode picker or a confirmation, the completions, the selection, a
    /// proposal being edited, the open story, then the drafting panel.
    /// Whether there was anything to close.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.discard_composer(window, cx) {
            return true;
        }
        if self.mode_menu.take().is_some()
            || self.confirm_delete.take().is_some()
            || self.bulk_mode_menu.take().is_some()
            || std::mem::take(&mut self.confirm_bulk_delete)
        {
            cx.notify();
            return true;
        }
        if let Some(open) = &self.open
            && open.completion.read(cx).is_open()
        {
            open.completion.update(cx, |menu, cx| menu.hide(cx));
            return true;
        }
        // The selection is cleared before anything else closes.
        if !self.selected.is_empty() {
            self.clear_selection(window, cx);
            return true;
        }
        if self.open_step.is_some() {
            self.close_step(window, cx);
            return true;
        }
        if let Some(editor) = self.editing.take() {
            self.focus_row(&editor.id, window, cx);
            return true;
        }
        if self.open.is_some() {
            self.close_story(window, cx);
            return true;
        }
        if self
            .drafting
            .as_ref()
            .is_some_and(|draft| draft.running.is_none())
            && self.proposals.is_empty()
        {
            self.drafting = None;
            self.focus.focus(window, cx);
            cx.notify();
            return true;
        }
        false
    }

    // --- The selection --------------------------------------------------------

    /// The list the story named so can be selected in: Draft, the Backlog,
    /// or Next sprint, the three that take moves.
    fn selectable(&self, name: &str) -> Option<(Section, usize)> {
        self.find(name)
            .filter(|(section, _)| LISTS.contains(section))
    }

    /// Ctrl/Cmd+click or X: the story toggles alone in or out of the
    /// selection, the rest kept.
    fn toggle_selected(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some((section, _)) = self.selectable(name) else {
            return;
        };
        if !self.selected.remove(name) {
            self.selected.insert(name.to_string());
        }
        self.select_from.insert(section.key(), name.to_string());
        cx.notify();
    }

    /// Shift+click: the range within its column from the card last clicked
    /// or toggled there to this one, inclusive, added to the selection; this
    /// one alone while nothing there was.
    fn select_range(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some((section, to)) = self.selectable(name) else {
            return;
        };
        let stories = &self.sections[section_ix(section)];
        let from = self
            .select_from
            .get(section.key())
            .and_then(|from| stories.iter().position(|story| story.name == *from))
            .unwrap_or(to);
        let range = from.min(to)..=from.max(to);
        let names: Vec<String> = stories[range].iter().map(|story| story.name.clone()).collect();
        self.selected.extend(names);
        cx.notify();
    }

    /// The selected stories in the selection's order: Draft's first, top to
    /// bottom, then the Backlog's, then Next sprint's.
    fn selected_in_order(&self) -> Vec<String> {
        LISTS
            .into_iter()
            .flat_map(|section| self.sections[section_ix(section)].iter())
            .filter(|story| self.selected.contains(&story.name))
            .map(|story| story.name.clone())
            .collect()
    }

    /// Those of the selection in `section`, in order.
    fn selected_in(&self, section: Section) -> Vec<String> {
        self.sections[section_ix(section)]
            .iter()
            .filter(|story| self.selected.contains(&story.name))
            .map(|story| story.name.clone())
            .collect()
    }

    /// A story deleted, or gone from Draft, the Backlog, and Next sprint, as when
    /// its sprint is sent, leaves the selection.
    fn keep_selection(&mut self) {
        let kept: std::collections::HashSet<String> = self
            .selected
            .iter()
            .filter(|name| self.selectable(name).is_some())
            .cloned()
            .collect();
        self.selected = kept;
        if self.selected.is_empty() {
            self.bulk_mode_menu = None;
            self.confirm_bulk_delete = false;
        }
    }

    /// Clear: the selection is emptied; the focus, were it on the bulk bar
    /// going away, goes back to the board.
    fn clear_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let on_bar = self.bulk_focus.contains_focused(window, cx);
        self.selected.clear();
        self.keep_selection();
        if on_bar {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// The bulk bar's moves: the selected stories in each of `from` move to
    /// the end of `to`, in the selection's order, still selected.
    fn move_selected(&mut self, from: &[Section], to: Section, cx: &mut Context<Self>) {
        let moving: Vec<String> =
            from.iter().flat_map(|&section| self.selected_in(section)).collect();
        for name in moving {
            self.move_to(&name, to, cx);
        }
    }

    /// The bulk bar's Change mode: every selected story takes `mode`, its
    /// steps keeping their own.
    fn set_selected_mode(&mut self, mode: SendMode, cx: &mut Context<Self>) {
        for name in self.selected_in_order() {
            self.set_mode(&name, mode, cx);
        }
        self.bulk_mode_menu = None;
        cx.notify();
    }

    /// The bulk bar's Duplicate: every copy just beneath its original, the
    /// originals still selected and the copies not.
    fn duplicate_selected(&mut self, cx: &mut Context<Self>) {
        for name in self.selected_in_order() {
            self.duplicate(&name, cx);
        }
    }

    /// The bulk bar's Delete, once confirmed: every selected story.
    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_bulk_delete = false;
        let focused = self.focused_row(window);
        for name in self.selected_in_order() {
            self.delete(&name, cx);
        }
        self.keep_selection();
        if self.bulk_focus.contains_focused(window, cx)
            || focused.is_some_and(|name| self.find(&name).is_none())
        {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// The card a single-story shortcut acts on, pressed while no card has
    /// focus, as on the board itself: the one under the pointer, if any.
    fn pointed_card(&self, window: &Window) -> Option<String> {
        let on_board = self.focus.is_focused(window)
            || self
                .section_focus
                .values()
                .any(|handle| handle.is_focused(window));
        on_board
            .then(|| self.hovered.clone())
            .flatten()
            .filter(|name| self.find(name).is_some())
    }

    /// Ctrl/Cmd+N: the composer at the foot of the list that has focus, the
    /// Draft, the Backlog, or Next sprint, else at the foot of Next sprint.
    fn new_story_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let section = self.focused_list(window, cx).unwrap_or(Section::Next);
        self.compose_at_foot(section, window, cx);
    }

    /// Draft, the Backlog, or Next sprint, while the focus is on its heading, one of
    /// its cards, its "Add story" line, or its composer.
    fn focused_list(&self, window: &Window, cx: &App) -> Option<Section> {
        let row = self.focused_row(window);
        LISTS
            .into_iter()
            .find(|&section| {
                self.heading_focus[section.key()].contains_focused(window, cx)
                    || row.as_ref().is_some_and(|row| {
                        *row == add_line(section)
                            || self.find(row).is_some_and(|(found, _)| found == section)
                    })
                    || self.composer.as_ref().is_some_and(|composer| {
                        composer.section == section
                            && composer.title.read(cx).focus_handle(cx).is_focused(window)
                    })
            })
    }

    /// Ctrl/Cmd+Enter: the story named so moves to Next sprint, or from it
    /// or from Draft to the Backlog; never sent, as no story is on its own.
    fn toggle_sprint(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.find(name).map(|(section, _)| section) {
            Some(Section::Next) => self.move_story(name, Section::Backlog, 0, cx),
            Some(Section::Draft) => self.move_to(name, Section::Backlog, cx),
            Some(Section::Backlog | Section::Done) => self.move_to(name, Section::Next, cx),
            _ => return,
        }
        self.focus_row(name, window, cx);
    }

    fn delete(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some((section, ix)) = self.find(name) else {
            return;
        };
        self.sections[section_ix(section)].remove(ix);
        self.row_focus.remove(name);
        self.selected.remove(name);
        if self.hovered.as_deref() == Some(name) {
            self.hovered = None;
        }
        if self.open.as_ref().is_some_and(|open| open.name == name) {
            self.open = None;
        }
        if self.confirm_delete.as_deref() == Some(name) {
            self.confirm_delete = None;
        }
        // Its own draft goes with it, its run stopped and what waits gone.
        if let Some(draft) = self.story_drafts.remove(name) {
            draft.cancel.cancel();
        }
        if self.notes.drafts.remove(name).is_some() {
            self.write_later(draft_file(&self.project_dir, name), None, cx);
        }
        self.rejected
            .retain(|rejected| rejected.owner.as_deref() != Some(name));
        self.write_later(story_file(&self.project_dir, name), None, cx);
        self.save_order(cx);
        cx.notify();
    }

    /// Asks, in its row, to delete the story named so.
    fn ask_delete(&mut self, name: &str, cx: &mut Context<Self>) {
        self.mode_menu = None;
        self.confirm_delete = Some(name.to_string());
        cx.notify();
    }

    /// Deletes the story asking to be, the focus going to the row beside.
    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.confirm_delete.take() else {
            return;
        };
        let next = self
            .row_after(&name, 1)
            .or_else(|| self.row_after(&name, -1));
        self.delete(&name, cx);
        match next {
            Some(next) => self.focus_row(&next, window, cx),
            None => self.focus.focus(window, cx),
        }
    }

    fn duplicate(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some((section, ix)) = self.find(name) else {
            return;
        };
        // Just beneath its original, in its list; only Send sprint and
        // Close sprint fill Current sprint and Done, so a duplicate of a
        // story there goes to the end of Next sprint.
        let copy = self.sections[section_ix(section)][ix].duplicate();
        let copy_name = copy.name.clone();
        let (list, at) = match section {
            Section::Draft | Section::Backlog | Section::Next => (section, ix + 1),
            Section::Current | Section::Done => {
                (Section::Next, self.sections[section_ix(Section::Next)].len())
            }
        };
        self.sections[section_ix(list)].insert(at, copy);
        self.save_story_named(&copy_name, cx);
        self.save_order(cx);
        cx.notify();
    }

    /// Moves the story named so to place `to` of `section`, counted as the
    /// section is before it is taken out.
    fn move_story(&mut self, name: &str, section: Section, to: usize, cx: &mut Context<Self>) {
        let Some((from_section, from)) = self.find(name) else {
            return;
        };
        // Only Send sprint moves a story into the current sprint, and only
        // Close sprint out of it.
        if from_section == Section::Current || section == Section::Current {
            return;
        }
        let to = if from_section == section && to > from {
            to - 1
        } else {
            to
        };
        let story = self.sections[section_ix(from_section)].remove(from);
        let stories = &mut self.sections[section_ix(section)];
        stories.insert(to.min(stories.len()), story);
        self.save_order(cx);
        cx.notify();
    }

    /// Moves the stories named so, in their order, into place `to` of
    /// `section`, counted as the section is before they are taken out.
    fn move_stories(
        &mut self,
        names: &[String],
        section: Section,
        to: usize,
        cx: &mut Context<Self>,
    ) {
        let movable = |found: Option<(Section, usize)>| {
            found.is_some_and(|(section, _)| LISTS.contains(&section))
        };
        if !matches!(section, Section::Draft | Section::Backlog | Section::Next)
            || !names.iter().all(|name| movable(self.find(name)))
        {
            return;
        }
        // Where they land among the cards that stay.
        let stories = &self.sections[section_ix(section)];
        let to = stories[..to.min(stories.len())]
            .iter()
            .filter(|story| !names.contains(&story.name))
            .count();
        let mut moving = Vec::new();
        for name in names {
            if let Some((from, ix)) = self.find(name) {
                moving.push(self.sections[section_ix(from)].remove(ix));
            }
        }
        let stories = &mut self.sections[section_ix(section)];
        let to = to.min(stories.len());
        stories.splice(to..to, moving);
        self.save_order(cx);
        cx.notify();
    }

    pub(crate) fn move_to(&mut self, name: &str, section: Section, cx: &mut Context<Self>) {
        let end = self.sections[section_ix(section)].len();
        self.move_story(name, section, end, cx);
    }

    /// Alt+Up or Alt+Down: the story moves one place within its column.
    pub(crate) fn shift_story(&mut self, name: &str, up: bool, cx: &mut Context<Self>) {
        let Some((section, ix)) = self.find(name) else {
            return;
        };
        if !matches!(section, Section::Draft | Section::Backlog | Section::Next) {
            return;
        }
        let len = self.sections[section_ix(section)].len();
        match up {
            true if ix > 0 => self.move_story(name, section, ix - 1, cx),
            false if ix + 1 < len => self.move_story(name, section, ix + 2, cx),
            _ => {}
        }
    }

    /// Alt+Left or Alt+Right: the story moves into the column beside, as
    /// dragging does, where that column takes it: from Draft to the end of
    /// the Backlog, from the Backlog to the end of Draft or of Next sprint,
    /// and from Next sprint to the end of the Backlog.
    fn shift_story_across(&mut self, name: &str, right: bool, cx: &mut Context<Self>) {
        match (self.find(name).map(|(section, _)| section), right) {
            (Some(Section::Draft), true) => self.move_to(name, Section::Backlog, cx),
            (Some(Section::Backlog), false) => self.move_to(name, Section::Draft, cx),
            (Some(Section::Backlog), true) => self.move_to(name, Section::Next, cx),
            (Some(Section::Next), false) => self.move_to(name, Section::Backlog, cx),
            _ => {}
        }
    }

    /// The columns that take drops, each with its stories' names in order.
    fn drop_columns(&self) -> Vec<(Section, Vec<String>)> {
        LISTS
            .into_iter()
            .map(|section| {
                let names = self.sections[section_ix(section)]
                    .iter()
                    .map(|story| story.name.clone())
                    .collect();
                (section, names)
            })
            .collect()
    }

    /// Chooses the gap the dragged story would land in from where the
    /// pointer is now; whether it changed.
    fn update_gap(&self) -> bool {
        let gap = self.dragged.borrow().as_ref().and_then(|(name, at)| {
            choose_gap(
                name,
                &self.drag_carried.borrow(),
                *at,
                &self.drop_columns(),
                &self.column_bounds.borrow(),
                &self.row_bounds.borrow(),
            )
        });
        let changed = self.gap.get() != gap;
        self.gap.set(gap);
        changed
    }

    /// A story dragged anywhere over the board: the gap follows the
    /// pointer, and held near an edge, the board or the column scrolls.
    fn drag_moved(&mut self, event: &DragMoveEvent<StoryDrag>, cx: &mut Context<Self>) {
        let drag = event.drag(cx);
        let (name, size, grab) = (drag.name.clone(), drag.size, drag.grab.get());
        let carried = drag.carried.clone();
        // Picked up: its place is left open at once, as tall as the cards
        // it carries together, so the column doesn't jump as it lifts, and
        // the other cards it carries leave theirs, their columns closing up.
        if self.lift.is_none()
            && let Some(from) = self.find(&name)
        {
            let height = carried.iter().fold(size.height, |height, other| {
                let other = self
                    .row_bounds
                    .borrow()
                    .get(other)
                    .map_or(size.height, |bounds| bounds.size.height);
                height + CARD_GAP + other
            });
            self.lift = Some(Lift {
                name: name.clone(),
                carried: carried.clone(),
                from,
                size,
                height,
                grab,
            });
            self.slots = vec![Slot {
                at: from,
                height,
                open: crate::animations::slide::Toward::at(true),
            }];
            *self.drag_carried.borrow_mut() = carried;
            cx.notify();
        }
        *self.dragged.borrow_mut() = Some((name, event.event.position));
        if self.update_gap() {
            cx.notify();
        }
        if !self.drag_scrolling && self.drag_scroll(Duration::ZERO, cx) {
            self.keep_drag_scrolling(cx);
        }
    }

    /// A story dropped on the board: it moves into the gap under the
    /// pointer, if there is one.
    fn drop_story(&mut self, drag: &StoryDrag, window: &Window, cx: &mut Context<Self>) {
        let pointer = window.mouse_position();
        *self.dragged.borrow_mut() = Some((drag.name.clone(), pointer));
        self.update_gap();
        let gap = self.gap.take();
        *self.dragged.borrow_mut() = None;
        self.drag_carried.borrow_mut().clear();
        // Over a gap, into the placeholder open there.
        let into = gap.and(self.slot_at.get());
        let lift = self.lift.take();
        self.slots.clear();
        if let Some((section, at)) = gap {
            if drag.carried.is_empty() {
                self.move_story(&drag.name, section, at, cx);
            } else {
                // The selection, in its order, all into that list there.
                let mut names = drag.carried.clone();
                let grabbed = self
                    .selected_in_order()
                    .iter()
                    .position(|name| *name == drag.name)
                    .unwrap_or(0)
                    .min(names.len());
                names.insert(grabbed, drag.name.clone());
                self.move_stories(&names, section, at, cx);
            }
        }
        if let Some(lift) = lift {
            self.settle(lift, pointer, into);
        }
        cx.notify();
    }

    /// The dragged card's copy, let go at `pointer`, settles into its
    /// place: into the placeholder at `into`, where it was dropped, or else
    /// back into the gap it came from, the cards there making room again.
    fn settle(&mut self, lift: Lift, pointer: Point<Pixels>, into: Option<Point<Pixels>>) {
        let Some(face) = self.card_face(&lift.name) else {
            return;
        };
        // Where it lands is where the placeholder was, until it is drawn.
        if let Some(into) = into {
            self.row_bounds
                .borrow_mut()
                .insert(lift.name.clone(), Bounds::new(into, lift.size));
        }
        self.slides.forget(&lift.name);
        self.slide_next.set(true);
        self.settle = Some(Settle {
            name: lift.name,
            face,
            size: lift.size,
            from: pointer - lift.grab,
            since: Instant::now(),
        });
    }

    /// While a card is dragged, opens the one placeholder where it would
    /// land, as the PlanningScope's motion says, closing any other: the gap
    /// chosen, or else, while the pointer is over its own column, the gap
    /// it came from.
    fn sync_slots(&mut self) {
        let Some((from, height)) = self.lift.as_ref().map(|lift| (lift.from, lift.height))
        else {
            return;
        };
        let pointer = self.dragged.borrow().as_ref().map(|(_, at)| *at);
        let home = pointer.is_some_and(|at| {
            self.column_bounds
                .borrow()
                .get(from.0.key())
                .is_some_and(|(column, _)| column.contains(&at))
        });
        let open = self.gap.get().or(home.then_some(from));
        for slot in &mut self.slots {
            slot.open.set(Some(slot.at) == open);
        }
        if let Some(at) = open
            && !self.slots.iter().any(|slot| slot.at == at)
        {
            let mut opening = crate::animations::slide::Toward::at(false);
            opening.set(true);
            self.slots.push(Slot {
                at,
                height,
                open: opening,
            });
        }
        self.slots
            .retain(|slot| slot.open.is_open() || slot.open.moving());
    }

    /// What `name`'s card shows, for a copy of it.
    fn card_face(&self, name: &str) -> Option<CardFace> {
        let story = self.story(name)?;
        let untitled = story.title.trim().is_empty();
        Some(CardFace {
            title: if untitled {
                "Untitled".into()
            } else {
                story.title.clone().into()
            },
            untitled,
            mode: story.mode(),
            from_agent: story.from_agent,
            steps: story.steps().len(),
            attached: story.has_attachments(),
            cited: !story.spec.is_empty(),
            updated: story.updated,
            count: 1,
        })
    }

    /// Scrolls the board, and the column under the pointer, for `elapsed`
    /// at the pace the dragged story held near their edges asks; whether
    /// either is held near an edge.
    fn drag_scroll(&mut self, elapsed: Duration, cx: &mut Context<Self>) -> bool {
        let Some(at) = self.dragged.borrow().as_ref().map(|(_, at)| *at) else {
            return false;
        };
        if !cx.has_active_drag() {
            return false;
        }
        let ms = elapsed.as_secs_f32() * 1000.;
        let mut held = false;
        let mut moved = false;
        if let Some(board) = self.board_bounds.get()
            && (board.top()..board.bottom()).contains(&at.y)
        {
            let speed = edge_speed(at.x, board.left(), board.right());
            held |= speed != 0.;
            moved |= scroll_by(&self.board_scroll, Axis::Horizontal, px(speed * ms));
        }
        let column = self
            .column_bounds
            .borrow()
            .iter()
            .find(|(_, (_, cards))| (cards.left()..cards.right()).contains(&at.x))
            .map(|(key, (_, cards))| (*key, *cards));
        if let Some((key, cards)) = column
            && let Some(handle) = self.column_scroll.get(key)
            && handle.max_offset().y > px(0.)
        {
            let speed = edge_speed(at.y, cards.top(), cards.bottom());
            held |= speed != 0.;
            moved |= scroll_by(handle, Axis::Vertical, px(speed * ms));
        }
        if moved {
            cx.notify();
        }
        held
    }

    /// A wheel turn or swipe over the board, as the PlanningScope's layout
    /// says: over a column, its up and down share scrolls only that
    /// column, and its sideways share, or the whole of it with Shift held,
    /// the board; outside every column, the board sideways, down moving it
    /// right. Where the board and its columns had scrolled to by
    /// themselves is undone first.
    fn wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some((board, columns)) = self.wheel_from.borrow_mut().take() else {
            return;
        };
        self.board_scroll.set_offset(board);
        for (key, offset) in columns {
            self.column_scroll[key].set_offset(offset);
        }
        let mut delta = event.delta.pixel_delta(window.line_height());
        if event.modifiers.shift && delta.x.is_zero() {
            delta = point(delta.y, px(0.));
        }
        let at = event.position;
        let column = self
            .column_bounds
            .borrow()
            .iter()
            .find(|(_, (column, _))| column.contains(&at))
            .map(|(key, _)| *key);
        let moved = match column {
            Some(key) => {
                let down = scroll_by(&self.column_scroll[key], Axis::Vertical, delta.y);
                scroll_by(&self.board_scroll, Axis::Horizontal, delta.x) | down
            }
            None => {
                let by = if delta.x.is_zero() { delta.y } else { delta.x };
                scroll_by(&self.board_scroll, Axis::Horizontal, by)
            }
        };
        if moved || board != self.board_scroll.offset() {
            cx.notify();
        }
    }

    /// While the dragged story is held near an edge, scrolls on each
    /// frame, stopping once it is moved away or let go.
    fn keep_drag_scrolling(&mut self, cx: &mut Context<Self>) {
        self.drag_scrolling = true;
        cx.spawn(async move |this, cx| {
            let mut last = Instant::now();
            loop {
                cx.background_executor().timer(DRAG_FRAME).await;
                let now = Instant::now();
                let elapsed = now - last;
                last = now;
                let held = this
                    .update(cx, |this, cx| {
                        let held = this.drag_scroll(elapsed, cx);
                        this.drag_scrolling = held;
                        held
                    })
                    .unwrap_or(false);
                if !held {
                    break;
                }
            }
        })
        .detach();
    }

    fn set_mode(&mut self, name: &str, mode: SendMode, cx: &mut Context<Self>) {
        if let Ok(mut last) = LAST_MODE.lock() {
            *last = Some(mode);
        }
        if let Some(story) = self.story_mut(name) {
            story.anchor.mode = Some(mode);
        }
        self.mode_menu = None;
        self.save_story_named(name, cx);
        cx.notify();
    }

    /// Opens the mode picker of the story named so, its mode highlighted.
    fn open_mode_menu(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(story) = self.story(name).filter(|_| {
            self.find(name)
                .is_some_and(|(section, _)| section != Section::Current)
        }) else {
            return;
        };
        let at = SendMode::ALL
            .iter()
            .position(|mode| *mode == story.mode())
            .unwrap_or(0);
        self.confirm_delete = None;
        self.mode_menu = Some((name.to_string(), at));
        cx.notify();
    }

    fn change(
        &mut self,
        name: &str,
        change: impl FnOnce(&mut HiddenAnchor),
        cx: &mut Context<Self>,
    ) {
        if let Some(story) = self.story_mut(name) {
            change(&mut story.anchor);
        }
        self.save_story_named(name, cx);
        cx.notify();
    }

    /// Attaches files picked to the story named so.
    fn attach(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        let (name, project_dir) = (name.to_string(), self.project_dir.clone());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let saved = cx
                .background_spawn(async move {
                    let (mut images, mut files) = (Vec::new(), Vec::new());
                    for path in paths {
                        match attached_file::load(&path) {
                            Ok(attached_file::Loaded::Image(image)) => images.push(image),
                            Ok(attached_file::Loaded::File(file)) => files.push(file),
                            Err(_) => {}
                        }
                    }
                    let images = attached_image::save_all(&images, &project_dir)?;
                    let files = attached_file::save_all(&files, &project_dir)?;
                    anyhow::Ok((images, files))
                })
                .await;
            this.update(cx, |this, cx| match saved {
                Ok((images, files)) => this.change(
                    &name,
                    |anchor| {
                        anchor.attached_images.extend(images);
                        anchor.attached_files.extend(files);
                    },
                    cx,
                ),
                Err(err) => eprintln!("could not attach the files: {err:#}"),
            })
            .ok();
        })
        .detach();
    }

    /// How the story stands, once sent, and how long its task has run while
    /// it runs.
    fn status(&self, story: &Story, cx: &App) -> Option<(StoryState, Option<Duration>)> {
        let task = story.task.as_deref()?;
        // A task that can't be found never arrives: missing, not queued.
        Some((self.hooks.state)(task, cx).unwrap_or((StoryState::Missing, None)))
    }

    fn state(&self, story: &Story, cx: &App) -> Option<StoryState> {
        self.status(story, cx).map(|(state, _)| state)
    }

    /// Sends the stories named so, in order, as one block, each with its
    /// steps after it.
    fn send_named(&mut self, names: &[String], cx: &mut Context<Self>) {
        let mut sends = Vec::new();
        for name in names {
            let project_dir = self.project_dir.clone();
            let Some(story) = self.story_mut(name) else {
                continue;
            };
            if story.text().trim().is_empty() {
                continue;
            }
            let task = crate::prompt_title::name_for(&project_dir, &story.title);
            story.task = Some(task.clone());
            story.from_agent = false;
            sends.push(StorySend {
                name: task,
                text: story.text(),
                mode: story.mode(),
                attached: story.anchor.attached(),
                sliced: story.anchor.sliced,
                model: story.anchor.model.clone(),
                effort: story.anchor.effort.clone(),
                steps: story.steps().to_vec(),
            });
        }
        if sends.is_empty() {
            return;
        }
        self.save_order(cx);
        (self.hooks.send)(sends, cx);
        cx.notify();
    }

    /// Sends Next sprint, as the PlanningScope's sprint says: every story in
    /// it, in order, as one block, moved into Current sprint. Nothing is
    /// sent while a sprint is under way, or while planning is paused for
    /// review; and no story is ever sent on its own.
    pub fn send_sprint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending().held().is_some() {
            return;
        }
        self.close_story(window, cx);
        self.start_sprint(cx);
    }

    /// Sends Next sprint's stories, moving them into Current sprint: those
    /// with something to send, as a story with neither title nor prompt has
    /// not. Gives how many were sent.
    fn start_sprint(&mut self, cx: &mut Context<Self>) -> usize {
        self.waiting_start = false;
        let (sendable, rest): (Vec<Story>, Vec<Story>) = std::mem::take(&mut self.sections[0])
            .into_iter()
            .partition(|story| !story.text().trim().is_empty());
        // Only what is sent goes into Current sprint: an empty story stays.
        self.left_empty = rest.len();
        self.sections[0] = rest;
        if sendable.is_empty() {
            cx.notify();
            return 0;
        }
        // A story sent is no longer written in place.
        if self
            .open
            .as_ref()
            .is_some_and(|open| sendable.iter().any(|story| story.name == open.name))
        {
            self.open = None;
            self.open_step = None;
        }
        let names: Vec<String> = sendable.iter().map(|story| story.name.clone()).collect();
        self.sections[3].extend(sendable);
        self.send_named(&names, cx);
        self.save_order(cx);
        cx.notify();
        names.len()
    }

    /// Turns Start automatically on or off, saved with the plan. Off, a
    /// sprint already sent carries on; on, the next may start at once.
    fn set_auto_start(&mut self, on: bool, cx: &mut Context<Self>) {
        self.notes.auto_start = on;
        self.waiting_start = false;
        self.save_order(cx);
        cx.notify();
    }

    /// Why Next sprint isn't starting by itself, while Start automatically
    /// is on and it would otherwise: paused for review, or the current
    /// sprint ended with a story not done.
    fn auto_held(&self, cx: &App) -> Option<&'static str> {
        if !self.notes.auto_start {
            return None;
        }
        if self.notes.paused.is_some() {
            return Some(
                "Planning is paused for review, so the next sprint waits for Resume or Approve.",
            );
        }
        if !self.sprint_ended(cx) {
            return None;
        }
        let states: Vec<Option<StoryState>> = self.sections[3]
            .iter()
            .map(|story| self.state(story, cx))
            .collect();
        if states.contains(&Some(StoryState::Missing)) {
            return Some(
                "A story's task can't be found, so the next sprint waits: close the sprint to go on.",
            );
        }
        states
            .iter()
            .any(|state| *state != Some(StoryState::Done))
            .then_some(
                "A story of the current sprint didn't finish, so the next sprint waits: close the sprint to go on.",
            )
    }

    /// Starts Next sprint by itself, as the PlanningScope's auto start
    /// says, when Start automatically is on: once every story of Current
    /// sprint ended Done, it is closed and Next sprint sent; with no sprint
    /// current, Next sprint is sent once it holds a story. Never while
    /// paused, after a sprint that ended with a story not done, while a
    /// story of Next sprint is being written, or for an empty Next sprint.
    pub fn auto_advance(&mut self, cx: &mut Context<Self>) {
        if !self.notes.auto_start || self.notes.paused.is_some() || self.advancing {
            return;
        }
        // Found waiting as the project opened, it waits for Start now; once
        // Next sprint is empty, a story added starts it as ever.
        if self.waiting_start {
            if self.sections[section_ix(Section::Current)].is_empty() && self.has_sendable() {
                return;
            }
            self.waiting_start = false;
        }
        if !self.sections[3].is_empty() {
            let all_done = self.sprint_ended(cx)
                && self.sections[3]
                    .iter()
                    .all(|story| self.state(story, cx) == Some(StoryState::Done));
            if !all_done {
                return;
            }
        }
        let writing = self
            .open
            .as_ref()
            .and_then(|open| self.find(&open.name))
            .is_some_and(|(section, _)| section == Section::Next);
        let sendable = self.has_sendable();
        if writing || !sendable {
            // A sprint all done still closes by itself.
            if !self.sections[3].is_empty() && !writing {
                self.advancing = true;
                self.close_sprint(cx);
                self.advancing = false;
            }
            return;
        }
        self.advancing = true;
        if !self.sections[3].is_empty() {
            self.close_sprint(cx);
        }
        let count = self.start_sprint(cx);
        self.advancing = false;
        if count == 0 {
            return;
        }
        let said = format!(
            "Next sprint started: {count} {}",
            if count == 1 { "story" } else { "stories" }
        );
        crate::debug_log::log(Some(&self.project_dir), format!("{said}, by itself"));
        notify_started(said, cx);
    }

    /// Whether Next sprint holds a story with something to send.
    fn has_sendable(&self) -> bool {
        self.sections[section_ix(Section::Next)]
            .iter()
            .any(|story| !story.text().trim().is_empty())
    }

    /// Start now: sends Next sprint, found waiting as the project opened.
    fn start_now(&mut self, cx: &mut Context<Self>) {
        self.waiting_start = false;
        if self.pending().held().is_none() {
            self.start_sprint(cx);
        }
        cx.notify();
    }

    /// Whether every story of the current sprint has ended.
    fn sprint_ended(&self, cx: &App) -> bool {
        !self.sections[3].is_empty()
            && self.sections[3]
                .iter()
                .all(|story| self.state(story, cx).is_some_and(StoryState::ended))
    }

    /// Closes the current sprint: the stories done go to Done; those that
    /// failed, were cancelled, or stopped go to the start of Next sprint, in
    /// their order, to be sent again.
    fn close_sprint(&mut self, cx: &mut Context<Self>) {
        let stories = std::mem::take(&mut self.sections[3]);
        let mut again = Vec::new();
        for mut story in stories {
            if self.state(&story, cx) == Some(StoryState::Done) {
                self.sections[2].push(story);
            } else {
                story.task = None;
                again.push(story);
            }
        }
        again.append(&mut self.sections[0]);
        self.sections[0] = again;
        self.save_order(cx);
        cx.notify();
    }

    // --- Research and the pause ----------------------------------------------

    /// Draft stories, from the Backlog's heading, Ctrl/Cmd+Shift+D, or the
    /// ribbon: the drafting panel, its request box focused.
    pub fn draft_stories(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_draft_panel(None, true, window, cx);
    }

    /// Research, from the Backlog's heading, Ctrl/Cmd+Shift+R, or the
    /// ribbon: the research panel, its box focused.
    pub fn research(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_research(None, window, cx);
    }

    /// Opens the panel to research a topic, beneath the Backlog's heading,
    /// or, from a story, beneath it.
    fn open_research(
        &mut self,
        about: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let running = self
            .drafting
            .as_ref()
            .is_some_and(|drafting| drafting.running.is_some());
        if !running {
            self.drafting = None;
        }
        self.open_draft_panel(about, true, window, cx);
        if let Some(drafting) = &mut self.drafting
            && drafting.running.is_none()
        {
            drafting.research = true;
        }
        cx.notify();
    }

    /// Research ended, however it ended: its findings are saved as a file
    /// of their own and listed, any proposals it ended with wait to be
    /// decided, and planning pauses for review, naming it.
    fn researched(
        &mut self,
        topic: String,
        about: Option<String>,
        reply: Result<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(drafting) = &mut self.drafting {
            drafting.running = None;
            drafting._run = None;
        }
        let ran_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let (state, body) = match reply {
            Ok(reply) => {
                for mut proposal in parse_proposals(&reply) {
                    proposal.about = about.clone();
                    self.settle_target(&mut proposal);
                    self.proposals.push(proposal);
                }
                (FindingState::Done, reply)
            }
            Err(err) if format!("{err:#}") == "stopped" => (
                FindingState::Stopped,
                "Stopped before it finished, so there are no findings.".to_string(),
            ),
            Err(err) => {
                // Shown in the panel too, with Try again, the question kept.
                if let Some(drafting) = &mut self.drafting {
                    drafting.error = Some(format!("{err:#}"));
                }
                (
                    FindingState::Failed,
                    format!("The research couldn't be done.\n\n```\n{err:#}\n```"),
                )
            }
        };
        let slug: String =
            crate::prompt_title::anchor_name(&topic).unwrap_or_else(|| "Research".into());
        // Never two of the same name, however quickly they come.
        let mut file = format!("{ran_at}-{slug}.md");
        let mut n = 2;
        while self
            .notes
            .findings
            .iter()
            .any(|finding| finding.file == file)
        {
            file = format!("{ran_at}-{slug}{n}.md");
            n += 1;
        }
        let text = format!("# Research: {topic}\n\n{body}\n");
        self.write_later(research_dir(&self.project_dir).join(&file), Some(text), cx);
        self.notes.findings.push(Finding {
            file: file.clone(),
            topic: topic.clone(),
            ran_at,
            state,
            story: about,
        });
        // Paused, or kept paused, naming the latest.
        self.notes.paused = Some(Pause {
            research: file,
            topic,
        });
        self.save_order(cx);
        cx.notify();
    }

    /// Ends the pause, as its Resume does.
    pub fn resume_planning(&mut self, cx: &mut Context<Self>) {
        self.notes.paused = None;
        self.save_order(cx);
        cx.notify();
    }

    /// Ends the pause and sends Next sprint at once, as its Approve and send
    /// sprint does.
    pub fn approve_and_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.resume_planning(cx);
        self.send_sprint(window, cx);
    }

    /// Opens the findings in `file` in a body tab of their own.
    fn open_findings(&self, file: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(finding) = self
            .notes
            .findings
            .iter()
            .find(|finding| finding.file == file)
        else {
            return;
        };
        let path = research_dir(&self.project_dir).join(&finding.file);
        (self.hooks.open_findings)(path, format!("Research: {}", finding.topic), window, cx);
    }

    /// Deletes the findings in `file`, their file with them.
    fn delete_findings(&mut self, file: &str, cx: &mut Context<Self>) {
        self.notes.findings.retain(|finding| finding.file != file);
        self.write_later(research_dir(&self.project_dir).join(file), None, cx);
        self.save_order(cx);
        cx.notify();
    }

    fn resume(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(task) = self.story(name).and_then(|story| story.task.clone()) {
            (self.hooks.resume)(&task, window, cx);
        }
    }

    // --- Spec citations ------------------------------------------------------

    /// Adds `text`, a link into the spec's Piton source, as one picked from
    /// Add citation, or "No match" and what was looked for, to the spec of
    /// the story named so, by hand. A link the Piton source doesn't define
    /// isn't added, and the open story says why. Whether it was added.
    fn add_citation(&mut self, name: &str, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(citation) = Citation::parse(text) else {
            return false;
        };
        let checked = citation
            .link
            .as_deref()
            .map_or(Ok(None), |link| check_link(link, &self.project_dir));
        if let Err(why) = checked {
            if let Some(open) = &mut self.open {
                open.spec_error = Some(format!("That link wasn't added: {why}."));
            }
            cx.notify();
            return false;
        }
        let Some(story) = self.story_mut(name).filter(|story| story.task.is_none()) else {
            return false;
        };
        if !story.spec.contains(&citation) {
            story.spec.push(citation);
        }
        if let Some(open) = &mut self.open {
            open.spec_error = None;
        }
        self.save_order(cx);
        cx.notify();
        true
    }

    /// Removes the citation at `at` from the story named so.
    fn remove_citation(&mut self, name: &str, at: usize, cx: &mut Context<Self>) {
        let Some(story) = self.story_mut(name).filter(|story| story.task.is_none()) else {
            return;
        };
        if at < story.spec.len() {
            story.spec.remove(at);
        }
        self.save_order(cx);
        cx.notify();
    }

    /// A list of citations, as a card or an open story shows them: each its
    /// kind, muted, then the element's name and its file, muted, as a link
    /// opening the Piton source at its declaration, with what it touches and
    /// the words it rests on; a No match muted with a small marker; with a
    /// remove button for the story named `editable`.
    fn render_citations(
        &self,
        key: &str,
        citations: &[Citation],
        editable: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let sent = editable
            .and_then(|name| self.story(name))
            .is_some_and(|story| story.task.is_some());
        let rows = citations.iter().enumerate().map(|(at, citation)| {
            let label = citation.label();
            let link = citation.link.clone();
            let row = h_flex()
                .gap_1p5()
                .items_center()
                .text_xs()
                .min_w_0()
                .when_some(citation.kind, |row, kind| {
                    row.child(div().flex_none().text_color(muted).child(kind.label()))
                });
            let row = match link {
                Some(link) => {
                    let project_dir = self.project_dir.clone();
                    let open = self.hooks.open_source.clone();
                    let file = citation.file().unwrap_or_default().to_string();
                    row.child(Icon::new(IconName::BookOpen).xsmall().text_color(muted))
                        .child(
                            div()
                                .id(SharedString::from(format!("planning-cite-{key}-{at}")))
                                .key_context("PlanningCitation")
                                .when_some(
                                    self.row_focus.get(&format!("cite:{key}:{at}")),
                                    |link, focus| link.track_focus(focus),
                                )
                                .flex_none()
                                .cursor_pointer()
                                .underline()
                                .rounded(px(3.))
                                .border_1()
                                .border_color(gpui_kit::transparent_black())
                                .focus(|link| link.border_color(theme.ring))
                                .text_color(theme.link)
                                .child(label)
                                .on_action({
                                    let (project_dir, open, link) =
                                        (project_dir.clone(), open.clone(), link.clone());
                                    move |_: &OpenCitation, window, cx| {
                                        open_citation(&link, &project_dir, &open, window, cx)
                                    }
                                })
                                .on_click(move |_, window, cx| {
                                    open_citation(&link, &project_dir, &open, window, cx)
                                }),
                        )
                        .child(div().min_w_0().truncate().text_color(muted).child(file))
                }
                None => row
                    .child(div().text_color(muted).child("∅"))
                    .child(div().flex_none().text_color(muted).child(label)),
            };
            let row = row
                .when(!citation.note.is_empty(), |row| {
                    row.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(muted)
                            .child(format!("— {}", citation.note)),
                    )
                })
                .when(!citation.quote.is_empty(), |row| {
                    row.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .italic()
                            .text_color(muted)
                            .child(format!("“{}”", citation.quote)),
                    )
                })
                .when_some(editable.filter(|_| !sent), |row, name| {
                    let name = name.to_string();
                    row.child(
                        Button::new(SharedString::from(format!("planning-uncite-{key}-{at}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .label("Remove")
                            .tooltip("Remove this citation")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_citation(&name, at, cx)
                            })),
                    )
                });
            gpui_kit::TestSupportExt::test_support(
                div()
                    .id(SharedString::from(format!("planning-citation-{key}-{at}")))
                    .child(row),
            )
        });
        v_flex()
            .gap_0p5()
            .children(rows)
            .when(citations.is_empty() && editable.is_some(), |list| {
                list.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Nothing cited yet."),
                )
            })
            .into_any_element()
    }

    // --- Steps ---------------------------------------------------------------

    /// Adds a step of `mode` holding `prompt` at the end of the story named
    /// so, its model and effort the story's, saved straight away: only to a
    /// story that exists, in the plan and in `.suspense/planning/`, and
    /// isn't sent. Otherwise nothing changes and nothing is saved, and the
    /// error says why. Gives the step's place among the story's steps.
    pub fn add_step(
        &mut self,
        name: &str,
        mode: SendMode,
        prompt: String,
        cx: &mut Context<Self>,
    ) -> Result<usize, String> {
        let on_disk = story_file(&self.project_dir, name).exists();
        let Some(story) = self.story_mut(name).filter(|_| on_disk) else {
            return Err(NO_SUCH_STORY.into());
        };
        if story.task.is_some() {
            return Err(STORY_SENT.into());
        }
        let mode = if mode == SendMode::Spec {
            SendMode::Spec
        } else {
            SendMode::Code
        };
        story.anchor.added_steps.push(AddedStep {
            mode,
            prompt,
            model: story.anchor.model.clone(),
            effort: story.anchor.effort.clone(),
        });
        let at = story.anchor.added_steps.len() - 1;
        self.save_story_named(name, cx);
        cx.notify();
        Ok(at)
    }

    /// Add step: a step at the end of the story named so, in the story's
    /// mode where that is Code or Spec, open and focused. Where it can't be
    /// added, says why beneath the steps, keeping what was to be added.
    fn add_step_here(
        &mut self,
        name: &str,
        kept: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = self.story(name).map_or(SendMode::Code, Story::mode);
        self.close_step(window, cx);
        match self.add_step(name, mode, kept.clone(), cx) {
            Ok(at) => {
                self.step_error = None;
                self.open_step(name, at, window, cx);
            }
            Err(why) => {
                self.step_error = Some((name.to_string(), why, kept));
                cx.notify();
            }
        }
    }

    /// Opens the step at `at` of the story named so to edit in place, its
    /// prompt focused.
    fn open_step(&mut self, name: &str, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .open_step
            .as_ref()
            .is_some_and(|step| step.story == name && step.at == at)
        {
            return;
        }
        self.close_step(window, cx);
        let Some(step) = self.story(name).and_then(|story| story.steps().get(at)) else {
            return;
        };
        let text = step.prompt.clone();
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, PROMPT_MAX_ROWS)
                .placeholder("What this step does… (Enter adds another)")
                .default_value(text)
        });
        let subscription = cx.subscribe_in(&prompt, window, {
            let name = name.to_string();
            move |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    let at = this.open_step.as_ref().map_or(at, |step| step.at);
                    this.change(
                        &name,
                        |anchor| {
                            if let Some(step) = anchor.added_steps.get_mut(at) {
                                step.prompt = value;
                            }
                        },
                        cx,
                    );
                }
            }
        });
        prompt.update(cx, |input, cx| input.focus(window, cx));
        self.open_step = Some(StepEditor {
            story: name.to_string(),
            at,
            prompt,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Closes the step being edited, its row focused again; one left with
    /// nothing in it is removed, as an empty step adds none.
    fn close_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keep_step_text(cx);
        let Some(step) = self.open_step.take() else {
            return;
        };
        let empty = self
            .story(&step.story)
            .and_then(|story| story.steps().get(step.at))
            .is_some_and(|added| added.prompt.trim().is_empty());
        if empty {
            self.remove_step(&step.story, step.at, cx);
        } else {
            let handle = self.step_focus(&step.story, step.at, cx);
            handle.focus(window, cx);
        }
        cx.notify();
    }

    /// Keeps what the open step's prompt holds in its step, as typed.
    fn keep_step_text(&mut self, cx: &mut Context<Self>) {
        let Some(step) = self.open_step.as_ref() else {
            return;
        };
        let (story, at) = (step.story.clone(), step.at);
        let text = step.prompt.read(cx).value().to_string();
        let changed = self
            .story(&story)
            .and_then(|found| found.steps().get(at))
            .is_some_and(|added| added.prompt != text);
        if changed {
            self.change(
                &story,
                |anchor| {
                    if let Some(step) = anchor.added_steps.get_mut(at) {
                        step.prompt = text;
                    }
                },
                cx,
            );
        }
    }

    /// Enter in a step's prompt: one with something in it is kept and
    /// another starts after it; an empty one adds none.
    fn step_entered(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.open_step.as_ref() else {
            return;
        };
        let (story, at) = (step.story.clone(), step.at);
        self.keep_step_text(cx);
        let filled = self
            .story(&story)
            .and_then(|found| found.steps().get(at))
            .is_some_and(|added| !added.prompt.trim().is_empty());
        if filled {
            self.add_step_here(&story, String::new(), window, cx);
        } else {
            self.close_step(window, cx);
        }
    }

    /// Removes the step at `at` of the story named so; a sent story's steps
    /// read as they were sent.
    fn remove_step(&mut self, name: &str, at: usize, cx: &mut Context<Self>) {
        if self.story(name).is_none_or(|story| story.task.is_some()) {
            return;
        }
        if self
            .open_step
            .as_ref()
            .is_some_and(|step| step.story == name && step.at == at)
        {
            self.open_step = None;
        }
        self.change(
            name,
            |anchor| {
                if at < anchor.added_steps.len() {
                    anchor.added_steps.remove(at);
                }
            },
            cx,
        );
    }

    /// Moves the step at `from` of the story named so to `to`, among its
    /// steps only: the story's own prompt stays first.
    fn move_step(&mut self, name: &str, from: usize, to: usize, cx: &mut Context<Self>) {
        let Some(story) = self.story(name).filter(|story| story.task.is_none()) else {
            return;
        };
        let count = story.steps().len();
        if from >= count || to >= count || from == to {
            return;
        }
        if let Some(step) = &mut self.open_step
            && step.story == name
            && step.at == from
        {
            step.at = to;
        }
        self.change(
            name,
            |anchor| {
                let step = anchor.added_steps.remove(from);
                anchor.added_steps.insert(to, step);
            },
            cx,
        );
    }

    fn step_focus(&mut self, name: &str, at: usize, cx: &mut App) -> FocusHandle {
        self.step_focus
            .entry((name.to_string(), at))
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    /// The step of the open story whose row has the focus.
    fn focused_step(&self, window: &Window) -> Option<(String, usize)> {
        self.step_focus
            .iter()
            .find(|(_, handle)| handle.is_focused(window))
            .map(|(key, _)| key.clone())
    }

    /// Alt+Up or Alt+Down on a focused step: it moves one place among the
    /// steps, the focus going with it.
    fn shift_step(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((name, at)) = self.focused_step(window) else {
            return;
        };
        let to = if up { at.checked_sub(1) } else { Some(at + 1) };
        let Some(to) = to.filter(|to| {
            self.story(&name)
                .is_some_and(|story| *to < story.steps().len())
        }) else {
            return;
        };
        self.move_step(&name, at, to, cx);
        let handle = self.step_focus(&name, to, cx);
        handle.focus(window, cx);
    }

    // --- Drafting and proposals ---------------------------------------------

    /// Opens the drafting panel beneath the Backlog's heading, or, `about`
    /// a story, beneath it; focused, it takes the request.
    fn open_draft_panel(
        &mut self,
        about: Option<String>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // It takes the open story's place in the side panel.
        self.close_story(window, cx);
        let same = self
            .drafting
            .as_ref()
            .is_some_and(|drafting| drafting.about == about);
        if !same
            && self
                .drafting
                .as_ref()
                .is_none_or(|drafting| drafting.running.is_none())
        {
            let placeholder = if about.is_some() {
                "What this story still needs…"
            } else {
                "In a sentence or a paragraph…"
            };
            let input = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(2, DRAFT_MAX_ROWS)
                    .placeholder(placeholder)
            });
            let typed = cx.observe(&input, |_, _, cx| cx.notify());
            self.drafting = Some(DraftPanel {
                input,
                about,
                running: None,
                activity: Arc::default(),
                cancel: Arc::default(),
                error: None,
                details_open: false,
                proposed_nothing: false,
                research: false,
                _run: None,
                _typed: typed,
            });
        }
        if focus && let Some(drafting) = &self.drafting {
            drafting
                .input
                .update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Ask agent: drafting about the story named so.
    fn ask_agent(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_draft_panel(Some(name.to_string()), true, window, cx);
    }

    /// The stories proposals may add to, by id and title: every one not yet
    /// sent, the one asked about first.
    fn targets(&self, about: Option<&str>) -> Vec<(String, String)> {
        let mut targets: Vec<(String, String)> = self.sections[..2]
            .iter()
            .flatten()
            .filter(|story| story.task.is_none())
            .map(|story| (story.name.clone(), story.title.clone()))
            .collect();
        if let Some(about) = about
            && let Some(at) = targets.iter().position(|(name, _)| name == about)
        {
            let first = targets.remove(at);
            targets.insert(0, first);
        }
        targets
    }

    /// Asks the harness, reading the project and changing nothing, to
    /// propose stories for what the panel's request says.
    fn draft(&mut self, cx: &mut Context<Self>) {
        let Some(drafting) = &self.drafting else {
            return;
        };
        let wanted = drafting.input.read(cx).value().to_string();
        self.draft_wanted(wanted, cx);
    }

    /// Draft again, for a proposal that cited no spec: it goes, and the
    /// draft for that story runs once more.
    fn draft_again(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // One waiting for its story: that story drafts once more.
        if let Some(owner) = self.owner_of(id) {
            self.take_out(id);
            self.save_draft(&owner, cx);
            self.draft_story(&owner, cx);
            return;
        }
        let Some(proposal) = self.proposal(id).cloned() else {
            return;
        };
        self.proposals.retain(|kept| kept.id != id);
        self.save_order(cx);
        if self.drafting.is_none() {
            self.open_draft_panel(proposal.about.clone(), false, window, cx);
        }
        let wanted = format!(
            "This one story, grounded in the spec this time: search the spec for what it touches and cite it, or say No match.\n\nTitle: {}\n\n{}",
            proposal.story_title(),
            proposal.prompt
        );
        self.draft_wanted(wanted, cx);
    }

    /// Asks the harness to propose stories for `wanted`, as the panel's run.
    fn draft_wanted(&mut self, wanted: String, cx: &mut Context<Self>) {
        let Some(drafting) = &self.drafting else {
            return;
        };
        if wanted.trim().is_empty() || drafting.running.is_some() {
            return;
        }
        let about = drafting.about.clone();
        let research = drafting.research;
        let targets = self.targets(about.as_deref());
        let story = about.as_deref().and_then(|name| self.story(name));
        let reference = crate::hidden_anchor::reference_root(&self.project_dir);
        let spec = spec_location(&self.project_dir);
        let request = if research {
            research_request(&wanted, &targets, story, reference, &spec)
        } else {
            draft_request(&wanted, &targets, story, reference, &spec)
        };
        let Some(drafting) = &mut self.drafting else {
            return;
        };
        drafting.running = Some(Instant::now());
        drafting.error = None;
        drafting.details_open = false;
        drafting.proposed_nothing = false;
        drafting.cancel = Arc::default();
        *drafting.activity.lock().unwrap() = "Starting the harness…".into();
        let run = if research {
            self.hooks.research
        } else {
            self.hooks.draft
        };
        let project_dir = self.project_dir.clone();
        let (live, cancel) = (drafting.activity.clone(), drafting.cancel.clone());
        let task = cx.spawn(async move |this, cx| {
            let reply = cx
                .background_spawn(async move {
                    let tool = Mutex::new(String::new());
                    run(&project_dir, &request, &cancel, &|line| {
                        if let Some(said) = activity(&line, &mut tool.lock().unwrap()) {
                            *activity_of(&live) = said;
                        }
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                if research {
                    this.researched(wanted, about, reply, cx)
                } else {
                    this.drafted(about, reply.map(|reply| parse_proposals(&reply)), cx)
                }
            })
            .ok();
        });
        if let Some(drafting) = &mut self.drafting {
            drafting._run = Some(task);
        }
        self.keep_ticking(cx);
        cx.notify();
    }

    /// Stops the run, whatever it would have proposed never shown; research
    /// stopped still ends, its findings saying so, and pauses planning.
    fn stop_draft(&mut self, cx: &mut Context<Self>) {
        let Some(drafting) = &mut self.drafting else {
            return;
        };
        drafting.cancel.cancel();
        drafting._run = None;
        let was_running = drafting.running.take().is_some();
        if drafting.research && was_running {
            let topic = drafting.input.read(cx).value().to_string();
            let about = drafting.about.clone();
            self.researched(topic, about, Err(anyhow::anyhow!("stopped")), cx);
        }
        cx.notify();
    }

    // --- Drafting with the agent ---------------------------------------------

    /// The proposals waiting for the story named so, from its own drafts.
    fn waiting(&self, name: &str) -> &[Proposal] {
        self.notes
            .drafts
            .get(name)
            .map_or(&[], |draft| draft.proposals.as_slice())
    }

    /// Whether the story named so is drafting with the agent now.
    fn story_drafting(&self, name: &str) -> bool {
        self.story_drafts
            .get(name)
            .is_some_and(|draft| draft.running.is_some())
    }

    /// Saves the story named so's own draft in the background, its file
    /// removed once nothing waits and no run goes.
    fn save_draft(&self, name: &str, cx: &mut Context<Self>) {
        let text = self
            .notes
            .drafts
            .get(name)
            .filter(|draft| draft.running || !draft.proposals.is_empty())
            .map(|draft| serde_json::to_string_pretty(draft).unwrap_or_default());
        self.write_later(draft_file(&self.project_dir, name), text, cx);
    }

    /// Draft with agent: each story named drafts in a run of its own, all
    /// at once, none waiting on another.
    fn draft_with_agent(&mut self, names: &[String], cx: &mut Context<Self>) {
        for name in names {
            self.draft_story(name, cx);
        }
    }

    /// Drafts the story named so with the agent, as "Ask agent" drafts an
    /// open story but with nothing typed: a run of its own with no session,
    /// told the story and asked to turn it into a full story, split into new
    /// stories where it holds more than one piece of work. Only a story
    /// in Draft, the Backlog, or Next sprint drafts, and one already drafting
    /// starts no second run.
    fn draft_story(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.selectable(name).is_none() || self.story_drafting(name) {
            return;
        }
        let Some(story) = self.story(name) else {
            return;
        };
        let targets = self.targets(Some(name));
        let reference = crate::hidden_anchor::reference_root(&self.project_dir);
        let spec = spec_location(&self.project_dir);
        // Written in system-prompts/planning.pi.
        let request = draft_request(
            crate::baked_prompts::planning::STORY,
            &targets,
            Some(story),
            reference,
            &spec,
        );
        let (run, project_dir) = (self.hooks.draft, self.project_dir.clone());
        let started = Instant::now();
        let draft = self.story_drafts.entry(name.to_string()).or_default();
        draft.running = Some(started);
        draft.error = None;
        draft.details_open = false;
        draft.proposed_nothing = false;
        draft.cancel = Arc::default();
        draft.written = Arc::default();
        let (cancel, written) = (draft.cancel.clone(), draft.written.clone());
        let story_name = name.to_string();
        draft._run = Some(cx.spawn(async move |this, cx| {
            let reply = cx
                .background_spawn(async move {
                    run(&project_dir, &request, &cancel, &|line| {
                        let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                            return;
                        };
                        for event in crate::harness::parse(&event) {
                            if let crate::harness::HarnessEvent::TextDelta(text) = event {
                                activity_of(&written).push_str(&text);
                            }
                        }
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                let reply = reply.map(|reply| parse_proposals(&reply));
                this.story_drafted(&story_name, started, reply, cx)
            })
            .ok();
        }));
        // Saved as going, so one cut off as the application quits says so.
        self.notes.drafts.entry(name.to_string()).or_default().running = true;
        self.save_draft(name, cx);
        cx.notify();
    }

    /// The story named so's run, started at `started`, ended: what it
    /// proposed waits for the story, after what already did, or its
    /// failure shows on its card.
    fn story_drafted(
        &mut self,
        name: &str,
        started: Instant,
        reply: Result<Vec<Proposal>>,
        cx: &mut Context<Self>,
    ) {
        // Only the run going now; one stopped has ended already.
        let Some(draft) = self
            .story_drafts
            .get_mut(name)
            .filter(|draft| draft.running == Some(started))
        else {
            return;
        };
        draft.running = None;
        draft._run = None;
        match reply {
            Ok(proposals) => self.add_waiting(name, proposals, cx),
            Err(err) => {
                draft.error = Some(format!("{err:#}"));
                if let Some(saved) = self.notes.drafts.get_mut(name) {
                    saved.running = false;
                }
                self.save_draft(name, cx);
                cx.notify();
            }
        }
    }

    /// Stop, in a drafting story's menu: its run alone stops, whatever it
    /// had proposed by then kept.
    fn stop_story_draft(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(draft) = self.story_drafts.get_mut(name) else {
            return;
        };
        if draft.running.take().is_none() {
            return;
        }
        draft.cancel.cancel();
        draft._run = None;
        let written = activity_of(&draft.written).clone();
        self.add_waiting(name, parse_proposals(finished_blocks(&written)), cx);
    }

    /// `proposals` from the story named so's own run wait for it, after
    /// those already waiting, each a new story in the order given, saved
    /// straight away. A story in Draft as its run ends has nothing to
    /// accept: each is created at the Backlog's end straight away, in the
    /// order given, and the story marked "Drafted", only those failing the
    /// spec grounding waiting.
    fn add_waiting(&mut self, name: &str, proposals: Vec<Proposal>, cx: &mut Context<Self>) {
        if self.story(name).is_none() {
            return;
        }
        let proposed_nothing = proposals.is_empty();
        let mut settled: Vec<Proposal> = draft_proposals(name, proposals)
            .into_iter()
            .map(|mut proposal| {
                self.settle_target(&mut proposal);
                proposal
            })
            .collect();
        if self
            .find(name)
            .is_some_and(|(section, _)| section == Section::Draft)
        {
            let (grounded, ungrounded): (Vec<_>, Vec<_>) = settled
                .into_iter()
                .partition(|proposal| proposal.target.is_none() && !proposal.spec.is_empty());
            settled = ungrounded;
            for proposal in &grounded {
                self.add_proposed(proposal, cx);
            }
            if !grounded.is_empty() {
                let story = self.story_mut(name).expect("found just now");
                story.drafted += grounded.len();
                self.save_story_named(name, cx);
                self.save_order(cx);
            }
        }
        let saved = self.notes.drafts.entry(name.to_string()).or_default();
        saved.running = false;
        saved.proposals.extend(settled);
        let none_waiting = saved.proposals.is_empty();
        if let Some(draft) = self.story_drafts.get_mut(name) {
            draft.proposed_nothing = proposed_nothing && none_waiting;
        }
        self.save_draft(name, cx);
        cx.notify();
    }

    fn drafted(
        &mut self,
        about: Option<String>,
        reply: Result<Vec<Proposal>>,
        cx: &mut Context<Self>,
    ) {
        let Some(drafting) = &mut self.drafting else {
            return;
        };
        drafting.running = None;
        drafting._run = None;
        match reply {
            Ok(proposals) if proposals.is_empty() => drafting.proposed_nothing = true,
            Ok(proposals) => {
                // One about a story proposes only new stories.
                let proposals = match &about {
                    Some(about) => draft_proposals(about, proposals),
                    None => proposals,
                };
                for mut proposal in proposals {
                    proposal.about = about.clone();
                    self.settle_target(&mut proposal);
                    self.proposals.push(proposal);
                }
                self.save_order(cx);
            }
            Err(err) => drafting.error = Some(format!("{err:#}")),
        }
        cx.notify();
    }

    /// A proposal naming a story it can't be added to, one that no longer
    /// exists or was sent, becomes a new story, saying why.
    fn settle_target(&self, proposal: &mut Proposal) {
        self.check_citations(proposal);
        self.settle_story_target(proposal);
    }

    /// Checks each link a proposal cites, dropping any the Piton source
    /// doesn't define, with a note of which and why.
    fn check_citations(&self, proposal: &mut Proposal) {
        check_citations(proposal, &self.project_dir);
    }

    fn settle_story_target(&self, proposal: &mut Proposal) {
        // No proposal rewrites a story: one that would is a new story.
        proposal.rewrite = false;
        proposal.follow_up = false;
        let Some(target) = proposal.target.clone() else {
            return;
        };
        let why = match self.story(&target) {
            None => "The story it named no longer exists, so it would be a new story.",
            Some(story) if story.task.is_some() => {
                "The story it named has been sent, so it would be a new story."
            }
            Some(_) => return,
        };
        proposal.target = None;
        proposal.note = Some(why.into());
    }

    /// Takes in a proposal from elsewhere, as an Ask answer's card, to be
    /// edited in the panel before it is accepted.
    pub fn take_proposal(
        &mut self,
        mut proposal: Proposal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settle_target(&mut proposal);
        let id = proposal.id.clone();
        if self.proposal(&id).is_none() {
            self.proposals.push(proposal);
            self.save_order(cx);
        }
        self.open_draft_panel(None, false, window, cx);
        self.edit_proposal(&id, window, cx);
    }

    /// Drafts a proposal from elsewhere again, as an Ask answer's card that
    /// cited no spec does with Draft again: in the panel, grounded this time.
    pub fn draft_again_outside(
        &mut self,
        mut proposal: Proposal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settle_target(&mut proposal);
        let id = proposal.id.clone();
        if self.proposal(&id).is_none() {
            self.proposals.push(proposal);
        }
        self.draft_again(&id, window, cx);
    }

    /// Accepts a proposal from elsewhere, as an Ask answer's card does:
    /// recorded in the plan, or, where it can't be, kept in the panel to be
    /// accepted again, and why.
    pub fn accept_outside(
        &mut self,
        mut proposal: Proposal,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        self.settle_target(&mut proposal);
        let id = proposal.id.clone();
        if self.proposal(&id).is_none() {
            self.proposals.push(proposal);
        }
        self.accept(&id, cx)
    }

    /// Opens the story named so where it is, as an accepted card's link
    /// does.
    pub fn reveal_story(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.story(name).is_some() {
            self.show_story(name, window, cx);
        }
    }

    /// Accepts the proposal `id` into the plan, saved straight away: a new
    /// story at the Backlog's end, marked "From agent", its follow-up step
    /// its own, or a step at the end of the story it names, which is marked
    /// "Updated by agent" and, were it in Draft, moves to the Backlog's end.
    /// Where that story no longer exists, or was sent, nothing is changed:
    /// the proposal stays, made a new story, saying why. Gives the story it
    /// went to.
    pub(crate) fn accept(&mut self, id: &str, cx: &mut Context<Self>) -> Result<String, String> {
        let Some(proposal) = self.proposal(id).cloned() else {
            return Err("That proposal was already decided.".into());
        };
        if proposal.spec.is_empty() {
            return Err(
                "The drafter cited no spec, so this isn't a story yet: draft it again.".into(),
            );
        }
        let (story, note) = match &proposal.target {
            None => (self.add_proposed(&proposal, cx), "Added to Backlog".to_string()),
            Some(target) => {
                let mode = match (proposal.mode(), self.story(target).map(Story::mode)) {
                    (SendMode::Spec, _) => SendMode::Spec,
                    (SendMode::Code, _) => SendMode::Code,
                    (_, Some(SendMode::Spec)) => SendMode::Spec,
                    _ => SendMode::Code,
                };
                let target = target.clone();
                match self.add_step(&target, mode, proposal.prompt.clone(), cx) {
                    Ok(at) => {
                        let story = self.story_mut(&target).expect("added to just now");
                        story.updated = true;
                        story.highlight = Some(at);
                        for citation in &proposal.spec {
                            if !story.spec.contains(citation) {
                                story.spec.push(citation.clone());
                            }
                        }
                        (target.clone(), format!("Added to {}", story.title))
                    }
                    Err(why) => {
                        if let Some(kept) = self.proposal_mut(id) {
                            kept.target = None;
                            kept.note = Some("It would now be a new story.".into());
                        }
                        self.refused.insert(id.to_string(), why.clone());
                        self.save_proposal(id, cx);
                        cx.notify();
                        return Err(why);
                    }
                }
            }
        };
        let owner = self.take_out(id).and_then(|(_, owner, _)| owner);
        if let Some(owner) = &owner {
            self.save_draft(owner, cx);
        }
        self.refused.remove(id);
        if self.editing.as_ref().is_some_and(|editor| editor.id == id) {
            self.editing = None;
        }
        self.decided.push(Decided {
            text: note,
            story: story.clone(),
            owner,
        });
        // A story in Draft given a step moves to the Backlog's end, staying
        // open if it was; a new story from a draft never moves the drafted.
        if proposal.target.is_some()
            && self
                .find(&story)
                .is_some_and(|(section, _)| section == Section::Draft)
        {
            self.move_to(&story, Section::Backlog, cx);
        }
        self.save_order(cx);
        self.close_panel_when_decided(cx);
        cx.notify();
        Ok(story)
    }

    /// A new story at the Backlog's end, as the proposal `proposal` says,
    /// marked "From agent", its follow-up step its own, saved. Gives its
    /// name.
    fn add_proposed(&mut self, proposal: &Proposal, cx: &mut Context<Self>) -> String {
        let mut story = Story::new(
            proposal.story_title(),
            proposal.prompt.clone(),
            proposal.mode(),
            &self.project_dir,
        );
        story.from_agent = true;
        story.spec = proposal.spec.clone();
        // Its follow-up step its own, a Spec or Code step as any is.
        if let Some(step) = &proposal.step {
            story.anchor.added_steps.push(AddedStep {
                mode: if step.mode() == SendMode::Spec {
                    SendMode::Spec
                } else {
                    SendMode::Code
                },
                prompt: step.prompt.clone(),
                model: story.anchor.model.clone(),
                effort: story.anchor.effort.clone(),
            });
        }
        let name = story.name.clone();
        self.sections[section_ix(Section::Backlog)].push(story);
        self.save_story_named(&name, cx);
        name
    }

    /// Rejects the proposal `id`: its card goes, an Undo in its place for a
    /// few seconds.
    pub(crate) fn reject(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some((proposal, owner, at)) = self.take_out(id) else {
            return;
        };
        if self.editing.as_ref().is_some_and(|editor| editor.id == id) {
            self.editing = None;
        }
        match &owner {
            Some(owner) => self.save_draft(owner, cx),
            None => self.save_order(cx),
        }
        self.rejected.push(Rejected {
            proposal,
            owner,
            at,
            when: Instant::now(),
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_FOR).await;
            this.update(cx, |this, cx| {
                this.rejected
                    .retain(|rejected| rejected.when.elapsed() < UNDO_FOR);
                this.close_panel_when_decided(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Brings back the proposal `id`, rejected moments ago, where it was.
    fn undo_reject(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(at) = self
            .rejected
            .iter()
            .position(|rejected| rejected.proposal.id == id)
        else {
            return;
        };
        let rejected = self.rejected.remove(at);
        match rejected.owner {
            // Its story deleted meanwhile, it went with it.
            Some(owner) if self.find(&owner).is_some() => {
                let waiting = &mut self.notes.drafts.entry(owner.clone()).or_default().proposals;
                waiting.insert(rejected.at.min(waiting.len()), rejected.proposal);
                self.save_draft(&owner, cx);
            }
            Some(_) => {}
            None => {
                let place = rejected.at.min(self.proposals.len());
                self.proposals.insert(place, rejected.proposal);
                self.save_order(cx);
            }
        }
        cx.notify();
    }

    /// The ids of the proposals waiting in the drafting panel, or, given
    /// one, for that story.
    fn proposal_ids(&self, owner: Option<&str>) -> Vec<String> {
        let proposals = match owner {
            Some(owner) => self.waiting(owner),
            None => &self.proposals,
        };
        proposals.iter().map(|proposal| proposal.id.clone()).collect()
    }

    fn accept_all(&mut self, owner: Option<&str>, cx: &mut Context<Self>) {
        for id in self.proposal_ids(owner) {
            self.accept(&id, cx).ok();
        }
    }

    fn reject_all(&mut self, owner: Option<&str>, cx: &mut Context<Self>) {
        for id in self.proposal_ids(owner) {
            self.reject(&id, cx);
        }
    }

    /// The panel closes itself once no proposal is left to decide, nor any
    /// rejected one waiting on its Undo.
    fn close_panel_when_decided(&mut self, cx: &mut Context<Self>) {
        let running = self
            .drafting
            .as_ref()
            .is_some_and(|drafting| drafting.running.is_some());
        if self.proposals.is_empty() && !self.panel_rejected() && !running {
            self.drafting = None;
            if self
                .editing
                .as_ref()
                .is_some_and(|editor| self.owner_of(&editor.id).is_none())
            {
                self.editing = None;
            }
            self.decided.retain(|decided| decided.owner.is_some());
            cx.notify();
        }
    }

    /// Opens the proposal `id` for editing in its card: its title, prompt,
    /// mode, and target, changed before it is accepted.
    pub(crate) fn edit_proposal(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(proposal) = self.proposal(id).cloned() else {
            return;
        };
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Story title")
                .default_value(proposal.title.clone())
        });
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, PROMPT_MAX_ROWS)
                .default_value(proposal.prompt.clone())
        });
        let subscriptions = vec![
            cx.subscribe_in(&title, window, {
                let id = id.to_string();
                move |this, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let value = input.read(cx).value().to_string();
                        if let Some(proposal) = this.proposal_mut(&id) {
                            proposal.title = value;
                        }
                        this.save_proposal(&id, cx);
                        cx.notify();
                    }
                }
            }),
            cx.subscribe_in(&prompt, window, {
                let id = id.to_string();
                move |this, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let value = input.read(cx).value().to_string();
                        if let Some(proposal) = this.proposal_mut(&id) {
                            proposal.prompt = value;
                        }
                        this.save_proposal(&id, cx);
                        cx.notify();
                    }
                }
            }),
        ];
        title.update(cx, |input, cx| input.focus(window, cx));
        self.editing = Some(ProposalEditor {
            id: id.to_string(),
            title,
            prompt,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn set_proposal(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut Proposal),
        cx: &mut Context<Self>,
    ) {
        if let Some(proposal) = self.proposal_mut(id) {
            change(proposal);
            proposal.note = None;
        }
        self.refused.remove(id);
        self.save_proposal(id, cx);
        cx.notify();
    }

    // --- Rows and the keyboard ----------------------------------------------

    /// The rows in the order they are shown, by their stories' names and
    /// proposals' ids.
    /// The cards and rows each column of the board shows, by the names
    /// their focus is kept under, column by column, left to right.
    fn column_rows(&self) -> Vec<Vec<String>> {
        let mut columns = Vec::new();
        for section in SECTIONS {
            let mut rows: Vec<String> = self.sections[section_ix(section)]
                .iter()
                .map(|story| story.name.clone())
                .collect();
            // Its "Add story" line, after its last card.
            if matches!(section, Section::Draft | Section::Backlog | Section::Next) {
                rows.push(add_line(section));
            }
            columns.push(rows);
        }
        columns.push(
            self.notes
                .findings
                .iter()
                .rev()
                .map(|finding| format!("finding:{}", finding.file))
                .collect(),
        );
        columns
    }

    /// The story drafting or research is about, while it is about one.
    fn panel_about(&self) -> Option<String> {
        let about = match &self.drafting {
            Some(drafting) => drafting.about.clone(),
            None => self
                .proposals
                .first()
                .and_then(|proposal| proposal.about.clone()),
        }?;
        self.find(&about).is_some().then_some(about)
    }

    /// The row `step` rows on from the one named so, in its column.
    fn row_after(&self, name: &str, step: isize) -> Option<String> {
        let mut columns = self.column_rows();
        // The proposals, one beneath another in the side panel.
        columns.push(self.proposal_ids(None));
        // The open story's own, beneath it in the side panel.
        if let Some(open) = &self.open {
            columns.push(self.proposal_ids(Some(&open.name)));
        }
        let rows = columns
            .iter()
            .find(|rows| rows.iter().any(|row| row == name))?;
        let at = rows.iter().position(|row| row == name)? as isize + step;
        rows.get(usize::try_from(at).ok()?).cloned()
    }

    /// Left or Right on a focused row: the focus moves to the column
    /// beside, skipping any with nothing in it, onto its row nearest the
    /// focused one's height, and the board scrolls to show it.
    fn step_columns(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode_menu.is_some() {
            return;
        }
        let Some(name) = self.focused_row(window) else {
            return;
        };
        let columns = self.column_rows();
        let Some(from) = columns
            .iter()
            .position(|rows| rows.iter().any(|row| *row == name))
        else {
            return;
        };
        let bounds = self.row_bounds.borrow().clone();
        let height = bounds.get(&name).map(|at| at.center().y);
        let mut at = from as isize + step;
        while let Some(rows) = usize::try_from(at).ok().and_then(|at| columns.get(at)) {
            if rows.is_empty() {
                at += step;
                continue;
            }
            let nearest = match height {
                Some(height) => rows
                    .iter()
                    .min_by_key(|row| {
                        bounds
                            .get(*row)
                            .map_or(f32::MAX, |row| f32::from((row.center().y - height).abs()))
                            as i64
                    })
                    .cloned(),
                None => rows.first().cloned(),
            };
            if let Some(nearest) = nearest {
                self.board_scroll.scroll_to_item(at as usize);
                self.focus_row(&nearest, window, cx);
            }
            return;
        }
    }

    /// Starts the board, and the open story's column, scrolling just
    /// enough that its card is fully in view once the board has narrowed
    /// by `narrowing`, as the side panel opens: kept so only when it was
    /// fully in view before, unless it is to be shown. A card not yet drawn
    /// is waited for.
    fn reveal_open(&mut self, narrowing: Pixels, window: &mut Window) {
        if self.returning {
            return self.reveal_returning(narrowing);
        }
        let Some((name, how)) = self.reveal.take() else {
            return;
        };
        if self.open.as_ref().is_none_or(|open| open.name != name) {
            return;
        }
        let Some((section, _)) = self.find(&name) else {
            return;
        };
        let Some(card) = self.row_bounds.borrow().get(&name).copied() else {
            self.reveal = Some((name, how));
            window.request_animation_frame();
            return;
        };
        let column = &self.column_scroll[section.key()];
        let (board, list) = (self.board_scroll.bounds(), column.bounds());
        let in_view = card.left() >= board.left()
            && card.right() <= board.right()
            && card.top() >= list.top()
            && card.bottom() <= list.bottom();
        if how == Reveal::Keep && !in_view {
            return;
        }
        // Only as far as it must, the card at the nearest edge.
        let nearest = |start: Pixels, end: Pixels, from: Pixels, to: Pixels| {
            if end > to {
                (end - to).min(start - from).max(px(0.))
            } else if start < from {
                start - from
            } else {
                px(0.)
            }
        };
        let right = board.right() - narrowing;
        let sideways = nearest(card.left(), card.right(), board.left(), right);
        let down = match how {
            Reveal::Keep => px(0.),
            Reveal::Show => nearest(card.top(), card.bottom(), list.top(), list.bottom()),
        };
        let since = Instant::now();
        for (handle, axis, by) in [
            (self.board_scroll.clone(), Axis::Horizontal, sideways),
            (column.clone(), Axis::Vertical, down),
        ] {
            if by != px(0.) {
                self.glides.push(Glide {
                    handle,
                    axis,
                    by,
                    done: px(0.),
                    since,
                });
            }
        }
    }

    /// As its tab is selected again, after the side panel opened or closed
    /// while it was away: only the board's sideways scroll moves, at once,
    /// as far as the panel's opening needs to keep the open story's card in
    /// view, as the PlanningScope's layout says; every column keeps its own
    /// scroll, and a card not drawn before is left where it is.
    fn reveal_returning(&mut self, narrowing: Pixels) {
        let Some((name, _)) = self.reveal.take() else {
            return;
        };
        if narrowing <= px(0.) || self.open.as_ref().is_none_or(|open| open.name != name) {
            return;
        }
        let Some(card) = self.row_bounds.borrow().get(&name).copied() else {
            return;
        };
        let board = self.board_scroll.bounds();
        if card.left() < board.left() || card.right() > board.right() {
            return;
        }
        let right = board.right() - narrowing;
        let sideways = if card.right() > right {
            (card.right() - right).min(card.left() - board.left()).max(px(0.))
        } else {
            px(0.)
        };
        let offset = self.board_scroll.offset();
        self.board_scroll
            .set_offset(point((offset.x - sideways).min(px(0.)), offset.y));
    }

    /// Whether Planning's tab is the one the Body shows. Shown again, it
    /// comes back as it was left: whatever was still gliding is where it
    /// was going, and the next frame moves nothing.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if shown && !self.shown {
            self.returning = true;
            for glide in self.glides.drain(..) {
                let offset = glide.handle.offset();
                let rest = glide.by - glide.done;
                glide
                    .handle
                    .set_offset(offset.apply_along(glide.axis, |at| (at - rest).min(px(0.))));
            }
            cx.notify();
        }
        self.shown = shown;
    }

    /// Its tab opened anew: the board starts at its left edge and every
    /// column at its top, as the PlanningScope's layout says; the scroll
    /// is never kept from a tab closed before.
    pub fn forget_scroll(&mut self) {
        self.glides.clear();
        self.board_scroll.set_offset(Point::default());
        for handle in self.column_scroll.values() {
            handle.set_offset(Point::default());
        }
    }

    /// Moves each scroll gliding on as far as it has come by now.
    fn glide(&mut self, window: &mut Window) {
        for glide in &mut self.glides {
            let now = glide.by * crate::animations::slide::eased(glide.since);
            let step = now - glide.done;
            glide.done = now;
            let offset = glide.handle.offset();
            glide
                .handle
                .set_offset(offset.apply_along(glide.axis, |at| (at - step).min(px(0.))));
        }
        self.glides.retain(|glide| !crate::animations::slide::ended(glide.since));
        if !self.glides.is_empty() {
            window.request_animation_frame();
        }
    }

    fn row_focus(&mut self, name: &str, cx: &mut App) -> FocusHandle {
        self.row_focus
            .entry(name.to_string())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    fn focus_row(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.row_focus(name, cx);
        handle.focus(window, cx);
        cx.notify();
    }

    /// The story or proposal whose row has the focus.
    fn focused_row(&self, window: &Window) -> Option<String> {
        self.row_focus
            .iter()
            .find(|(_, handle)| handle.is_focused(window))
            .map(|(name, _)| name.clone())
    }

    /// Up or Down on a focused row: the mode picker's highlight moves while
    /// it is open, else the focus moves to the row beside.
    fn step_rows(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, at)) = &mut self.mode_menu {
            let count = SendMode::ALL.len() as isize;
            *at = (*at as isize + step).rem_euclid(count) as usize;
            cx.notify();
            return;
        }
        let Some(name) = self.focused_row(window) else {
            return;
        };
        if let Some(next) = self.row_after(&name, step) {
            self.focus_row(&next, window, cx);
        }
    }

    /// Enter on a focused row: picks the highlighted mode while the picker
    /// is open, deletes a story whose delete was asked for, else opens or
    /// closes the story.
    fn enter_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((name, at)) = self.mode_menu.clone() {
            self.set_mode(&name, SendMode::ALL[at], cx);
            return;
        }
        if self.confirm_delete.is_some() {
            return self.confirm_delete(window, cx);
        }
        let Some(name) = self.focused_row(window) else {
            return;
        };
        match LISTS
            .into_iter()
            .find(|&section| add_line(section) == name)
        {
            Some(section) => self.compose_at_foot(section, window, cx),
            None => self.toggle_story(&name, window, cx),
        }
    }

    /// The focus after a proposal is decided: the next row, else the one
    /// before, else Planning.
    fn focus_after_decision(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.row_after(id, 1).or_else(|| self.row_after(id, -1));
        match next {
            Some(next) if next != id => self.focus_row(&next, window, cx),
            _ => self.focus.focus(window, cx),
        }
    }

    /// Ticks once a second while something runs, so its time counts.
    fn keep_ticking(&mut self, cx: &mut Context<Self>) {
        if self._tick.is_some() {
            return;
        }
        self._tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let running = this
                    .update(cx, |this, cx| {
                        let running = this.any_running(cx)
                            || this
                                .drafting
                                .as_ref()
                                .is_some_and(|drafting| drafting.running.is_some());
                        if running {
                            cx.notify();
                        } else {
                            this._tick = None;
                        }
                        running
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));
    }

    fn any_running(&self, cx: &App) -> bool {
        self.sections
            .iter()
            .flatten()
            .any(|story| self.state(story, cx) == Some(StoryState::Running))
    }
}

/// `text` up to the end of its last fenced block closed, so a run stopped
/// midway keeps only the proposals it had finished.
fn finished_blocks(text: &str) -> &str {
    let (mut fences, mut end, mut at) = (0, 0, 0);
    for line in text.split_inclusive('\n') {
        at += line.len();
        if line.trim_start().starts_with("```") {
            fences += 1;
            if fences % 2 == 0 {
                end = at;
            }
        }
    }
    &text[..end]
}

/// The live line's text, however its lock was left.
fn activity_of(activity: &Mutex<String>) -> std::sync::MutexGuard<'_, String> {
    activity
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Every section, in the order the board shows them as columns, left to
/// right, as work moves.
const SECTIONS: [Section; 5] = [
    Section::Draft,
    Section::Backlog,
    Section::Next,
    Section::Current,
    Section::Done,
];

/// The lists stories are written in, moved among, and selected in, in the
/// selection's order: Draft, the Backlog, and Next sprint.
const LISTS: [Section; 3] = [Section::Draft, Section::Backlog, Section::Next];

/// The name an "Add story" line's focus is kept under, among its list's
/// cards.
fn add_line(section: Section) -> String {
    format!("add:{}", section.key())
}

fn section_ix(section: Section) -> usize {
    match section {
        Section::Next => 0,
        Section::Backlog => 1,
        Section::Done => 2,
        Section::Current => 3,
        Section::Draft => 4,
    }
}

/// How wide a row's grip is.
const GRIP_WIDTH: Pixels = px(16.);

/// How wide a row's mode pill is.
const PILL_WIDTH: Pixels = px(72.);

/// How wide a row's status and time are.
const STATUS_WIDTH: Pixels = px(96.);

/// How tall a section's heading is.
const HEADING_HEIGHT: Pixels = px(32.);

/// How far apart rows are.
const ROW_GAP: Pixels = px(2.);
/// The side panel's width.
const PANEL_WIDTH: Pixels = px(400.);

/// How far a section's first row is beneath its heading.
const HEADING_GAP: Pixels = px(8.);

/// How large a column's status icon is.
const STATUS_ICON: Pixels = px(14.);

/// How far a list's heading and cards are inside its edges.
const LIST_PADDING: Pixels = px(8.);

/// How tall a card's property row is.
const PROPERTY_HEIGHT: Pixels = px(20.);

/// How tall the edge along a list's top is, in its column's colour.
const COLUMN_EDGE: Pixels = px(2.);

/// How strongly a list's top edge lays its column's colour over the list.
const COLUMN_EDGE_STRENGTH: f32 = 0.5;

/// How far a list's corners are rounded.
const LIST_RADIUS: Pixels = px(8.);

/// Which way a list's bevel faces: pressed, sunk into the board's base.
const LIST_BEVEL: crate::theme::Bevel = crate::theme::Bevel::Pressed;

/// The colour a column's status icon is in, `None` being Findings': Draft's
/// and the Backlog's tertiary text, Next sprint's secondary text, Current sprint's
/// the accent, Done's success, and Findings' secondary text.
fn column_color(section: Option<Section>, palette: &crate::theme::Palette) -> Hsla {
    crate::theme::color(match section {
        Some(Section::Draft | Section::Backlog) => palette.text_tertiary,
        Some(Section::Next) | None => palette.text_secondary,
        Some(Section::Current) => palette.accent,
        Some(Section::Done) => palette.success,
    })
}

/// The edge along a column's list's top: its icon's colour, laid over the
/// recessed surface at half strength.
fn column_edge(section: Option<Section>, palette: &crate::theme::Palette) -> Hsla {
    column_color(section, palette).opacity(COLUMN_EDGE_STRENGTH)
}

/// A column's status icon, as the PlanningScope's parts give it, `None`
/// being Findings'. With its list's top edge, it tells its column apart at a
/// glance, only repeating the column's name.
fn column_icon(section: Option<Section>, cx: &App) -> AnyElement {
    let palette = crate::theme::palette(cx);
    let tint = column_color(section, palette);
    let ring = || {
        div()
            .size(STATUS_ICON)
            .flex_none()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
    };
    match section {
        // A drawn pencil, never a circle, so it can't pass for the Backlog's.
        Some(Section::Draft) => Icon::new(IconName::Pencil)
            .with_size(STATUS_ICON)
            .text_color(tint)
            .into_any_element(),
        Some(Section::Backlog) => Icon::new(IconName::CircleDashed)
            .with_size(STATUS_ICON)
            .text_color(tint)
            .into_any_element(),
        Some(Section::Next) => Icon::new(IconName::Circle)
            .with_size(STATUS_ICON)
            .text_color(tint)
            .into_any_element(),
        // Half filled: its left half, inside its ring.
        Some(Section::Current) => ring()
            .border_1()
            .border_color(tint)
            .child(
                h_flex().size(px(8.)).child(
                    div()
                        .w(px(4.))
                        .h_full()
                        .rounded_tl(px(4.))
                        .rounded_bl(px(4.))
                        .bg(tint),
                ),
            )
            .into_any_element(),
        Some(Section::Done) => ring()
            .bg(tint)
            .child(
                Icon::new(IconName::Check)
                    .with_size(px(10.))
                    .text_color(crate::theme::color(palette.base)),
            )
            .into_any_element(),
        None => Icon::new(IconName::Search)
            .with_size(STATUS_ICON)
            .text_color(tint)
            .into_any_element(),
    }
}

/// A column's heading's name, in small semibold primary text, beside its
/// status icon.
fn column_name(section: Option<Section>, cx: &App) -> Div {
    h_flex()
        .flex_none()
        .gap_2()
        .items_center()
        .child(column_icon(section, cx))
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(cx.theme().foreground)
                .child(section.map_or("Findings", |section| section.label())),
        )
}

/// A mode's pill: its label, small, in its colour.
fn mode_pill_label(mode: SendMode, cx: &App) -> Div {
    let color = chat_input::mode_color(mode, cx);
    div()
        .flex_none()
        .px_1p5()
        .rounded(px(4.))
        .text_xs()
        .font_medium()
        .text_color(color)
        .bg(color.opacity(0.14))
        .child(mode_label(mode))
}

/// A sent story's status, as a task's is shown in the message list: its
/// coloured label, with a spinner and its time while it runs, and a pause
/// icon while paused.
fn status_label(state: StoryState, elapsed: Option<Duration>, cx: &App) -> AnyElement {
    use crate::theme::Hue;
    // Its task can't be found: muted, in the warning colour, saying so.
    if state == StoryState::Missing {
        return gpui_kit::TestSupportExt::test_support(div().id("planning-task-missing"))
            .text_xs()
            .text_color(cx.theme().warning)
            .tooltip(|window, cx| {
                gpui_kit::component::tooltip::Tooltip::new("Its task can't be found")
                    .build(window, cx)
            })
            .child(state.label())
            .into_any_element();
    }
    let hue = match state {
        StoryState::Queued => Hue::Grey,
        StoryState::Running => Hue::Amber,
        StoryState::Done => Hue::Green,
        StoryState::Failed => Hue::Red,
        StoryState::Cancelled => Hue::Orange,
        // The theme's warning colour, as a paused task's.
        StoryState::Paused | StoryState::Missing => Hue::Amber,
    };
    let muted = cx.theme().muted_foreground;
    let tag = crate::theme::tag(hue, cx).text_xs().child(
        h_flex()
            .gap_1()
            .items_center()
            .when(state == StoryState::Paused, |tag| {
                tag.child(Icon::new(IconName::Pause).xsmall())
            })
            .child(state.label()),
    );
    h_flex()
        .gap_1()
        .justify_end()
        .items_center()
        .when(state == StoryState::Running, |row| {
            row.child(Spinner::new().xsmall())
        })
        .child(tag)
        .children(
            elapsed
                .filter(|_| state == StoryState::Running)
                .map(|elapsed| {
                    div()
                        .text_xs()
                        .text_color(muted)
                        .font_features(crate::subagents::tabular_figures())
                        .child(crate::subagents::format_elapsed(elapsed))
                }),
        )
        .into_any_element()
}

/// Says, passively, that the next sprint started by itself, with a link
/// that opens Planning.
fn notify_started(said: String, cx: &mut App) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::notification::Notification;
    cx.defer(move |cx| {
        let Some(handle) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
            return;
        };
        handle
            .update(cx, |_, window, cx| {
                let note = Notification::info(said.clone()).content(|_, _, _| {
                    h_flex()
                        .pt_1()
                        .child(
                            Button::new("planning-started-open")
                                .ghost()
                                .xsmall()
                                .label("Open Planning")
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(OpenPlanning), cx)
                                }),
                        )
                        .into_any_element()
                });
                window.push_notification(note, cx);
            })
            .ok();
    });
}

/// `element`, taking the clicks on what it holds for itself, so the row
/// it sits in never sees them.
/// A story card's menu, as its menu button and a right-click open it, its
/// items acting on that story alone.
/// What a card's menu offers of drafting with the agent.
#[derive(Clone)]
enum DraftItem {
    /// Nothing: the card isn't in Draft, the Backlog, or Next sprint.
    None,
    /// "Draft with agent", for the stories named, the card's alone or, right
    /// clicked in a selection of several, the whole selection's.
    Draft(Vec<String>),
    /// "Stop", while the card's own run goes.
    Stop,
}

fn story_menu(
    menu: PopupMenu,
    this: &WeakEntity<Planning>,
    name: &str,
    section: Section,
    sent: bool,
    draft: DraftItem,
) -> PopupMenu {
    let item = |label: &'static str,
                act: fn(&mut Planning, &str, &mut Window, &mut Context<Planning>)| {
        let (this, name) = (this.clone(), name.to_string());
        PopupMenuItem::new(label).on_click(move |_, window, cx| {
            this.update(cx, |this, cx| act(this, &name, window, cx)).ok();
        })
    };
    let menu = match section {
        Section::Next => menu.item(item("Move to backlog (Ctrl+Enter)", |this, name, _, cx| {
            this.move_story(name, Section::Backlog, 0, cx)
        })),
        Section::Draft => menu.item(item("Move to backlog (Ctrl+Enter)", |this, name, _, cx| {
            this.move_to(name, Section::Backlog, cx)
        })),
        Section::Backlog => menu
            .item(item("Move to next sprint (Ctrl+Enter)", |this, name, _, cx| {
                this.move_to(name, Section::Next, cx)
            }))
            .item(item("Move to draft", |this, name, _, cx| {
                this.move_to(name, Section::Draft, cx)
            })),
        Section::Done => menu.item(item(
            "Move to next sprint (Ctrl+Enter)",
            |this, name, _, cx| this.move_to(name, Section::Next, cx),
        )),
        Section::Current => menu,
    };
    let menu = match draft {
        DraftItem::None => menu,
        DraftItem::Draft(names) => {
            let label = match names.len() {
                1 => "Draft with agent".to_string(),
                n => format!("Draft {n} with agent"),
            };
            let this = this.clone();
            menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                this.update(cx, |this, cx| this.draft_with_agent(&names, cx))
                    .ok();
            }))
        }
        DraftItem::Stop => menu.item(item("Stop", |this, name, _, cx| {
            this.stop_story_draft(name, cx)
        })),
    };
    let menu = if sent {
        menu
    } else {
        menu.item(item("Ask agent", |this, name, window, cx| {
            this.ask_agent(name, window, cx)
        }))
        .item(item("Research", |this, name, window, cx| {
            this.open_research(Some(name.to_string()), window, cx)
        }))
    };
    menu.item(item("Duplicate", |this, name, _, cx| this.duplicate(name, cx)))
        .separator()
        .item(item("Delete…", |this, name, _, cx| this.ask_delete(name, cx)))
}

fn own_clicks(element: Div) -> Div {
    element.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// An empty state: one muted line, centred in its column, saying what is
/// missing and what to do.
fn empty_line(text: String, muted: Hsla) -> Div {
    div()
        .w_full()
        .min_h(ROW_HEIGHT)
        .flex()
        .items_center()
        .justify_center()
        .text_center()
        .text_sm()
        .text_color(muted)
        .child(text)
}

/// Keeps where the card or row it is laid over was drawn, under `name`.
fn record_bounds(
    rows: Rc<std::cell::RefCell<std::collections::HashMap<String, Bounds<Pixels>>>>,
    name: String,
) -> impl IntoElement {
    canvas(
        move |bounds, _, _| {
            rows.borrow_mut().insert(name, bounds);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Something in a column, with how much of it is there, from 0 for nothing,
/// as a lifted card, to 1 for all of it, so a placeholder opening or closing
/// takes as much of the gap beside it as of its own height.
type Spaced = (AnyElement, f32);

/// How far beyond the view a column lays its cards out, so they don't pop
/// in as it scrolls.
const CARD_OVERDRAW: Pixels = px(320.);

/// How tall a card counts before any of its column's cards was measured.
const CARD_GUESS: Pixels = px(72.);

/// How long a frame may spend measuring cards out of view, and the most it
/// measures, so no one frame lays out every card of a long column.
const CARD_MEASURE_BUDGET: Duration = Duration::from_millis(4);
const CARD_MEASURE_MOST: usize = 16;

/// How tall a column's cards are, as they were last laid out, so the column
/// lays out only the cards in view and a few either side, as a table's rows
/// are, the rest counted at their height, and its scrollbar holds still.
/// Cards out of view not yet measured at the column's width are measured a
/// few a frame, until every one is known.
#[derive(Default)]
struct ColumnCards {
    /// The width cards were last laid out at.
    width: Option<Pixels>,
    /// Each card's height, by its story, or by what else the column holds,
    /// and whether it was measured at the current width.
    heights: std::collections::HashMap<String, (Pixels, bool)>,
    /// The heights measured at the current width, summed, and how many, for
    /// a card not yet measured to count as tall as they are on average.
    sum: Pixels,
    measured: usize,
    /// The stories whose cards were drawn last frame.
    drawn: std::collections::HashSet<String>,
}

impl ColumnCards {
    /// How tall a card not yet measured counts.
    fn guess(&self) -> Pixels {
        if self.measured == 0 {
            CARD_GUESS
        } else {
            self.sum / self.measured as f32
        }
    }

    fn height(&self, key: &str) -> Option<Pixels> {
        self.heights.get(key).map(|(height, _)| *height)
    }

    /// Whether `key` is measured at the current width.
    fn exact(&self, key: &str) -> bool {
        self.heights.get(key).is_some_and(|(_, exact)| *exact)
    }

    /// `key` was laid out `height` tall, `width` wide; any other width wraps
    /// every card anew, so each is measured again.
    fn record(&mut self, key: &str, height: Pixels, width: Pixels) {
        if self.width != Some(width) {
            self.width = Some(width);
            for (_, exact) in self.heights.values_mut() {
                *exact = false;
            }
            self.sum = px(0.);
            self.measured = 0;
        }
        if let Some((was, true)) = self.heights.get(key) {
            self.sum -= *was;
            self.measured -= 1;
        }
        self.heights.insert(key.to_string(), (height, true));
        self.sum += height;
        self.measured += 1;
    }
}

/// `element`, its height kept in `column` by `key` as it is laid out.
fn measured(element: AnyElement, column: Rc<std::cell::RefCell<ColumnCards>>, key: String) -> AnyElement {
    div()
        .w_full()
        .flex_none()
        .child(element)
        .child(
            canvas(
                move |bounds, _, _| {
                    column
                        .borrow_mut()
                        .record(&key, bounds.size.height, bounds.size.width)
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .relative()
        .into_any_element()
}

/// What a column holds, in order, before it is drawn: a story's card, by
/// its place, drawn only while it is in view; or anything else, drawn
/// always, with how much of the gap above it there is, the key its height
/// is kept by, and its height where it is known without laying it out.
enum ColumnEntry {
    Card { ix: usize, open: bool },
    Drawn(AnyElement, f32, Option<&'static str>, Option<Pixels>),
}

/// Where each column was last drawn, by its key: the whole column, and the
/// part its cards scroll in.
type ColumnBounds = std::collections::HashMap<&'static str, (Bounds<Pixels>, Bounds<Pixels>)>;

/// Where the board, and each column by its key, were scrolled to.
type WheelFrom = (Point<Pixels>, Vec<(&'static str, Point<Pixels>)>);

/// The gap a story named `dragged`, carrying the stories `carried`, with
/// the pointer `at`, would land in: among the cards of the column the
/// pointer is over, before the first whose middle is below it, else after
/// the last, and none in a column that takes no drops, nor, carrying none,
/// either side of the story itself. `columns` are those that take drops,
/// each with its stories' names in order.
fn choose_gap(
    dragged: &str,
    carried: &[String],
    at: Point<Pixels>,
    columns: &[(Section, Vec<String>)],
    column_bounds: &ColumnBounds,
    row_bounds: &std::collections::HashMap<String, Bounds<Pixels>>,
) -> Option<(Section, usize)> {
    let (section, names) = columns.iter().find(|(section, _)| {
        column_bounds
            .get(section.key())
            .is_some_and(|(column, _)| column.contains(&at))
    })?;
    // The dragged cards are lifted out, its place the gap it left.
    let gap = names
        .iter()
        .enumerate()
        .filter(|(_, name)| *name != dragged && !carried.contains(name))
        .find(|(_, name)| {
            row_bounds
                .get(*name)
                .is_some_and(|row| row.center().y > at.y)
        })
        .map_or(names.len(), |(ix, _)| ix);
    match names.iter().position(|name| name == dragged) {
        Some(from) if carried.is_empty() && (gap == from || gap == from + 1) => None,
        _ => Some((*section, gap)),
    }
}

/// How fast, in pixels a millisecond, an area running from `start` to
/// `end` scrolls with a dragged card held `at` along it: none unless within
/// [`DRAG_EDGE`] of an edge, or past it, then faster the nearer the edge or
/// the further past it, up to [`DRAG_MOST_SPEED`]. Positive toward `start`.
fn edge_speed(at: Pixels, start: Pixels, end: Pixels) -> f32 {
    let toward_start = f32::from(start + DRAG_EDGE - at);
    let toward_end = f32::from(at - (end - DRAG_EDGE));
    let reach = 2. * f32::from(DRAG_EDGE);
    if toward_start > 0. {
        DRAG_MOST_SPEED * (toward_start / reach).min(1.)
    } else if toward_end > 0. {
        -DRAG_MOST_SPEED * (toward_end / reach).min(1.)
    } else {
        0.
    }
}

/// Scrolls what `handle` scrolls `by` along `axis`, stopping at its ends;
/// whether it moved.
fn scroll_by(handle: &ScrollHandle, axis: Axis, by: Pixels) -> bool {
    let offset = handle.offset();
    let max = handle.max_offset().along(axis);
    let next = (offset.along(axis) + by).min(px(0.)).max(-max);
    if next == offset.along(axis) {
        return false;
    }
    handle.set_offset(offset.apply_along(axis, |_| next));
    true
}

/// A loading state: a spinner and a short muted label, where the result
/// will appear.
fn loading_line(label: String, muted: Hsla) -> Div {
    h_flex()
        .h(ROW_HEIGHT)
        .px_3()
        .gap_2()
        .items_center()
        .min_w_0()
        .text_sm()
        .text_color(muted)
        .child(Spinner::new().xsmall())
        .child(div().min_w_0().truncate().child(label))
}

/// `text` cut to its first `lines` lines, and whether anything was cut.
fn clipped(text: &str, lines: usize) -> (String, bool) {
    let all: Vec<&str> = text.lines().collect();
    if all.len() <= lines {
        return (text.to_string(), false);
    }
    (format!("{}…", all[..lines].join("\n")), true)
}

impl Planning {
    fn render_heading(
        &self,
        section: Section,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let count = self.sections[section_ix(section)].len();
        let paused = self.sections[section_ix(section)]
            .iter()
            .filter(|story| self.state(story, cx) == Some(StoryState::Paused))
            .count();
        let warning = crate::theme::Hue::Amber.of(crate::theme::palette(cx));
        let mut actions: Vec<AnyElement> = Vec::new();
        match section {
            Section::Current => {
                if self.sprint_ended(cx) {
                    actions.push(
                        Button::new("planning-close-sprint")
                            .ghost()
                            .small()
                            .label("Close sprint")
                            .tooltip(
                                "Move the stories done to Done, and any that didn't finish to Next sprint",
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.close_sprint(cx)))
                            .into_any_element(),
                    );
                }
            }
            Section::Next => {
                let pending = self.pending();
                actions.push(
                    div()
                        .id("planning-auto-start")
                        .key_context("PlanningSwitch")
                        .track_focus(&self.switch_focus)
                        .rounded(px(4.))
                        .border_1()
                        .border_color(gpui_kit::transparent_black())
                        .focus(|switch| switch.border_color(theme.ring))
                        .aria_label("Start automatically")
                        .on_action(cx.listener(|this, _: &ToggleSwitch, _, cx| {
                            this.set_auto_start(!this.notes.auto_start, cx)
                        }))
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(
                                "Start the next sprint as soon as the current one is done",
                            )
                            .build(window, cx)
                        })
                        .child(
                            gpui_kit::component::switch::Switch::new("planning-auto-start-switch")
                                .xsmall()
                                .label("Start automatically")
                                .checked(self.notes.auto_start)
                                .on_click(cx.listener(|this, on: &bool, _, cx| {
                                    this.set_auto_start(*on, cx)
                                })),
                        )
                        .into_any_element(),
                );
                actions.push(
                    Button::new("planning-send-sprint")
                        .primary()
                        .small()
                        .icon(IconName::Send)
                        .label("Send sprint")
                        .disabled(pending.held().is_some())
                        .tooltip(match (pending.held(), pending.waiting) {
                            (Some(why), _) => why.to_string(),
                            (None, 1) => format!("Send the one story ({MOD}Shift+Enter)"),
                            (None, n) => {
                                format!("Send the {n} stories, in order ({MOD}Shift+Enter)")
                            }
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.send_sprint(window, cx)))
                        .into_any_element(),
                );
            }
            Section::Backlog => {
                actions.push(
                    Button::new("planning-draft-stories")
                        .ghost()
                        .small()
                        .icon(IconName::Sparkles)
                        .label("Draft stories")
                        .tooltip("Describe what you want, and have the agent propose stories")
                        .on_click(cx.listener(|this, _, window, cx| this.draft_stories(window, cx)))
                        .into_any_element(),
                );
                actions.push(
                    Button::new("planning-research")
                        .ghost()
                        .small()
                        .icon(IconName::Search)
                        .label("Research")
                        .tooltip("Have the agent look into something, reading only, before anything is built")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.research(window, cx)),
                        )
                        .into_any_element(),
                );
            }
            // No actions but its "+ Add".
            Section::Draft | Section::Done => {}
        }
        // Lines beneath the heading's row, each on its own.
        let mut notes: Vec<AnyElement> = Vec::new();
        // What the last Send sprint left behind, while it is still there.
        let empty = self.sections[section_ix(Section::Next)]
            .iter()
            .filter(|story| story.text().trim().is_empty())
            .count()
            .min(self.left_empty);
        if section == Section::Next && empty > 0 {
            notes.push(
                gpui_kit::TestSupportExt::test_support(
                    div()
                        .id("planning-left-empty")
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(if empty == 1 {
                            "1 empty story left in Next sprint".to_string()
                        } else {
                            format!("{empty} empty stories left in Next sprint")
                        }),
                )
                .into_any_element(),
            );
        }
        // Found waiting as the project opened: started only by hand.
        if section == Section::Next
            && self.waiting_start
            && self.notes.auto_start
            && self.auto_held(cx).is_none()
        {
            notes.push(
                gpui_kit::TestSupportExt::test_support(
                    h_flex()
                        .id("planning-auto-start-waiting")
                        .gap_2()
                        .items_center()
                        .child(div().text_xs().text_color(muted).child("Waiting to start"))
                        .child(
                            Button::new("planning-start-now")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Play)
                                .label("Start now")
                                .tooltip("Send Next sprint now")
                                .on_click(cx.listener(|this, _, _, cx| this.start_now(cx))),
                        ),
                )
                .into_any_element(),
            );
        }
        // Why the next sprint isn't starting by itself, in its heading.
        if section == Section::Next
            && let Some(why) = self.auto_held(cx)
        {
            notes.push(
                gpui_kit::TestSupportExt::test_support(
                    div()
                        .id("planning-auto-start-held")
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(warning)
                        .child(why),
                )
                .into_any_element(),
            );
        }
        let name = column_name(Some(section), cx);
        let group = SharedString::from(format!("planning-heading-{}", section.key()));
        // "+ Add", just after the count, on Draft, the Backlog, and Next sprint: shown
        // while the heading is hovered or holds the focus, its room kept
        // whether shown or not, so the actions never shift.
        let add_top = self.heading_focus.get(section.key()).map(|heading| {
            let shown = heading.contains_focused(window, cx);
            let (ring, hover_bg, foreground) = (theme.ring, theme.list_hover, theme.foreground);
            gpui_kit::TestSupportExt::test_support(
                h_flex()
                    .id(SharedString::from(format!("planning-add-top-{}", section.key())))
                    .key_context("PlanningAdd")
                    .track_focus(&self.add_top_focus[section.key()])
                    .h(px(22.))
                    .px_1p5()
                    .gap_1()
                    .items_center()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .focus(move |add| add.border_color(ring))
                    .cursor_pointer()
                    .text_xs()
                    .text_color(muted)
                    .hover(move |add| add.bg(hover_bg).text_color(foreground))
                    .aria_label("Add")
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(
                            "Add a story at the top of the list",
                        )
                        .build(window, cx)
                    })
                    .when(!shown, |add| {
                        add.invisible().group_hover(group.clone(), |add| add.visible())
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.compose_at_top(section, window, cx)
                    }))
                    .on_action(cx.listener(move |this, _: &OpenComposer, window, cx| {
                        this.compose_at_top(section, window, cx)
                    }))
                    .child(Icon::new(IconName::Plus).xsmall())
                    .child("Add"),
            )
        });
        // Its status icon, name, and count, and its actions at its right, wrapping onto a
        // line beneath its name rather than being cut off.
        let row = h_flex()
            .w_full()
            .min_h(HEADING_HEIGHT)
            .flex_wrap()
            .gap_x_2()
            .gap_y_1()
            .items_center()
            .child(
                h_flex()
                    .h(HEADING_HEIGHT)
                    .flex_none()
                    .gap_2()
                    .items_center()
                    .child(name)
                    .child(div().text_xs().text_color(muted).child(count.to_string()))
                    .children(add_top)
                    // Start automatically on, beside Next sprint's count.
                    .when(section == Section::Next && self.notes.auto_start, |row| {
                        row.child(gpui_kit::TestSupportExt::test_support(
                            div()
                                .id("planning-auto-badge")
                                .text_xs()
                                .font_medium()
                                .text_color(theme.accent)
                                .child("Auto"),
                        ))
                    })
                    .when(paused > 0, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(warning)
                                .child(format!("{paused} paused")),
                        )
                    }),
            )
            .when(!actions.is_empty(), |row| {
                row.child(
                    h_flex()
                        .ml_auto()
                        .flex_wrap()
                        .gap_2()
                        .items_center()
                        .justify_end()
                        .children(actions),
                )
            });
        let heading = v_flex()
            .id(SharedString::from(format!("planning-heading-{}", section.key())))
            .group(group)
            .w_full()
            .flex_none()
            .gap_1()
            .child(row)
            .children(notes);
        match self.heading_focus.get(section.key()) {
            Some(focus) => heading.track_focus(focus).into_any_element(),
            None => heading.into_any_element(),
        }
    }

    /// A column of the board, as the PlanningScope's layout says: `heading`
    /// at its top, and beneath it `cards`, scrolling on their own, with the
    /// scrollbar along its right while they hold more than fits. Where it
    /// and its cards were drawn is kept, for a dragged card to find its gap
    /// and scroll it.
    fn render_column(
        &self,
        section: Option<Section>,
        key: &'static str,
        heading: AnyElement,
        cards: Vec<Spaced>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let handle = self.column_scroll[key].clone();
        let overflows = handle.max_offset().y > px(0.);
        // The cards its own children, so it can be scrolled to any of them,
        // each the gap below the one before it, as much of the gap as each
        // is there.
        let mut above: f32 = 0.;
        let body = v_flex()
            .id(SharedString::from(format!("planning-column-{key}-cards")))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&handle)
            .children(cards.into_iter().map(|(card, there)| {
                let gap = CARD_GAP * there * above.min(1.);
                above = above.max(there);
                div().w_full().flex_none().mt(gap).child(card)
            }));
        let body = if overflows {
            crate::scrollbar::with_scrollbar(
                format!("planning-column-{key}"),
                &handle,
                body,
                true,
                None,
                cx,
            )
        } else {
            body.into_any_element()
        };
        // Changes above the view never move the cards in it.
        let body = self.steady[key].hold(handle.clone(), self.slides.clone(), body);
        // Once the cards come to hold more than fits, or fit again, the
        // scrollbar is shown, or hidden, from the next frame.
        let this = cx.entity().downgrade();
        let watch = canvas(
            move |_, _, cx| {
                if (handle.max_offset().y > px(0.)) != overflows {
                    let this = this.clone();
                    cx.defer(move |cx| {
                        this.update(cx, |_, cx| cx.notify()).ok();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_0();
        let record = |cards: bool| {
            let columns = self.column_bounds.clone();
            canvas(
                move |bounds, _, _| {
                    let mut columns = columns.borrow_mut();
                    let at = columns.entry(key).or_insert((bounds, bounds));
                    if cards {
                        at.1 = bounds;
                    } else {
                        at.0 = bounds;
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };
        // A list, as Trello draws one: on the recessed surface, one step
        // from the board's base, its corners rounded, with no border, but
        // pressed into the base, and its column's colour along its top.
        let palette = crate::theme::palette(cx);
        let recessed = crate::theme::color(palette.recessed);
        let edge = column_edge(section, palette);
        gpui_kit::TestSupportExt::test_support(
            v_flex()
                .id(SharedString::from(format!("planning-column-{key}")))
                .track_focus(&self.section_focus[key])
                .relative()
                .w(COLUMN_WIDTH)
                .flex_none()
                .h_full()
                .min_h_0()
                .p(LIST_PADDING)
                .rounded(LIST_RADIUS)
                .bg(recessed)
                .child(crate::theme::rounded_bevel(LIST_BEVEL, LIST_RADIUS, cx))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .rounded(LIST_RADIUS)
                        .border_t(COLUMN_EDGE)
                        .border_color(edge),
                )
                .child(record(false))
                .child(div().flex_none().pb(HEADING_GAP).child(heading))
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .child(record(true))
                        .child(body),
                )
                .child(watch),
        )
        .into_any_element()
    }

    /// A section's column: its heading, and its cards.
    fn render_section(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let heading = self.render_heading(section, window, cx);
        let mut cards: Vec<AnyElement> = Vec::new();
        // While the plan is read: skeleton cards, never an empty column.
        if !self.loaded {
            if section == Section::Next
                && let Some(why) = self.load_error.clone()
            {
                cards.push(
                    gpui_kit::TestSupportExt::test_support(
                        h_flex()
                            .id("planning-load-error")
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().text_sm().text_color(theme.danger).child(why))
                            .child(
                                Button::new("planning-load-retry")
                                    .ghost()
                                    .xsmall()
                                    .label("Try again")
                                    .tooltip("Read the plan again")
                                    .on_click(cx.listener(|this, _, _, cx| this.read_again(cx))),
                            ),
                    )
                    .into_any_element(),
                );
            } else {
                cards.push(
                    gpui_kit::TestSupportExt::test_support(
                        v_flex()
                            .id(SharedString::from(format!(
                                "planning-loading-{}",
                                section.key()
                            )))
                            .gap(CARD_GAP)
                            .child(skeleton_card(cx))
                            .child(skeleton_card(cx)),
                    )
                    .into_any_element(),
                );
            }
            let cards = cards.into_iter().map(|card| (card, 1.)).collect();
            return self.render_column(Some(section), section.key(), heading, cards, cx);
        }
        let count = self.sections[section_ix(section)].len();
        let takes_drops = matches!(section, Section::Draft | Section::Backlog | Section::Next);
        // The composer, where its story will sit, the cards below it moved
        // down, and the list scrolled to keep it in view.
        let composing = self
            .composer
            .as_ref()
            .filter(|composer| composer.section == section)
            .map(|composer| composer.at.min(count));
        let mut composer_at = None;
        // The card lifted, and those it carries.
        let lifted: Vec<String> = self
            .lift
            .iter()
            .flat_map(|lift| std::iter::once(&lift.name).chain(&lift.carried))
            .cloned()
            .collect();
        let scrolls = vec![
            self.board_scroll.clone(),
            self.column_scroll[section.key()].clone(),
        ];
        let mut entries: Vec<ColumnEntry> = Vec::with_capacity(count + 2);
        for ix in 0..=count {
            if count > 0 && composing == Some(ix) {
                composer_at = Some(entries.len());
                entries.push(ColumnEntry::Drawn(self.render_composer(cx), 1., Some("\0composer"), None));
            }
            let story = self.sections[section_ix(section)].get(ix);
            let has_story = story.is_some();
            let open = story.is_some_and(|story| {
                self.open.as_ref().is_some_and(|open| open.name == story.name)
            });
            let lifted_here = story.is_some_and(|story| lifted.contains(&story.name));
            // Lifted, the card is kept, taking no room, where the gap it
            // left opens.
            if lifted_here {
                let row = self.render_row(section, ix, open, window, cx);
                entries.push(ColumnEntry::Drawn(
                    div().h_0().overflow_hidden().invisible().child(row).into_any_element(),
                    0.,
                    None,
                    Some(px(0.)),
                ));
            }
            // Each placeholder opening or closing before this place.
            for slot in self.slots.iter().filter(|slot| slot.at == (section, ix)) {
                let there = slot.open.now();
                let well = crate::theme::color(crate::theme::palette(cx).well);
                let record = slot.open.is_open().then(|| {
                    let slot_at = self.slot_at.clone();
                    canvas(move |bounds, _, _| slot_at.set(Some(bounds.origin)), |_, _, _, _| {})
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                });
                entries.push(ColumnEntry::Drawn(
                    div()
                        .relative()
                        .w_full()
                        .h(slot.height * there)
                        .rounded(px(6.))
                        .bg(well)
                        .children(record)
                        .into_any_element(),
                    there,
                    None,
                    Some(slot.height * there),
                ));
            }
            if has_story && !lifted_here {
                entries.push(ColumnEntry::Card { ix, open });
            }
        }
        if count == 0 {
            // Lets UI tests find the empty state; inert in normal builds.
            entries.push(ColumnEntry::Drawn(
                gpui_kit::TestSupportExt::test_support(
                    div()
                        .id(SharedString::from(format!("planning-empty-{}", section.key())))
                        .child(empty_line(section.empty(), muted)),
                )
                .into_any_element(),
                1.,
                Some("\0empty"),
                None,
            ));
            // With no cards, in the line's place, beneath the empty state.
            if composing.is_some() {
                composer_at = Some(entries.len());
                entries.push(ColumnEntry::Drawn(self.render_composer(cx), 1., Some("\0composer"), None));
            }
        }
        // The "Add story" line at its foot, after its last card.
        if takes_drops && composing.is_none_or(|at| at < count) {
            entries.push(ColumnEntry::Drawn(
                self.render_add_line(section, window, cx),
                1.,
                Some("\0add"),
                Some(px(32.)),
            ));
        }
        let (cards, composer_at) =
            self.lay_out_column(section, entries, composer_at, scrolls, window, cx);
        if let Some(at) = composer_at
            && let Some(composer) = self.composer.as_mut().filter(|composer| composer.reveal)
        {
            composer.reveal = false;
            self.column_scroll[section.key()].scroll_to_item(at);
        }
        self.render_column(Some(section), section.key(), heading, cards, cx)
    }

    /// What a column holds, as it is drawn: every story's card in view, and
    /// a few either side, and whatever else it holds, each the gap beneath
    /// the one before it, as [`Self::render_column`] spaces them; the cards
    /// out of view in their place, as tall as they were last laid out, never
    /// laid out themselves, as the TableDrawing says of a table's rows. The
    /// open story's card, the focused one, and one settling from a drop are
    /// drawn wherever they are, so they can be scrolled to and keep the
    /// keyboard. Cards out of view not yet measured are measured a few a
    /// frame. Where `composer_at` is among what is drawn, too.
    fn lay_out_column(
        &mut self,
        section: Section,
        entries: Vec<ColumnEntry>,
        composer_at: Option<usize>,
        scrolls: Vec<ScrollHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Vec<Spaced>, Option<usize>) {
        let key = section.key();
        let name_of = |this: &Self, ix: usize| this.sections[section_ix(section)][ix].name.clone();
        let column = self.column_cards[key].clone();
        let handle = self.column_scroll[key].clone();
        let focused = self.focused_row(window);
        let settling = self.settle.as_ref().map(|settle| settle.name.clone());
        let (tops, bottoms) = {
            let cards = column.borrow();
            let guess = cards.guess();
            let mut above: f32 = 0.;
            let mut at = px(0.);
            let mut tops = Vec::with_capacity(entries.len());
            let mut bottoms = Vec::with_capacity(entries.len());
            for entry in &entries {
                let (there, height) = match entry {
                    ColumnEntry::Card { ix, .. } => (
                        1.,
                        cards
                            .height(&self.sections[section_ix(section)][*ix].name)
                            .unwrap_or(guess),
                    ),
                    ColumnEntry::Drawn(_, there, key, known) => (
                        *there,
                        known
                            .or_else(|| key.and_then(|key| cards.height(key)))
                            .unwrap_or(px(32.)),
                    ),
                };
                at += CARD_GAP * there * above.min(1.);
                above = above.max(there);
                tops.push(at);
                at += height;
                bottoms.push(at);
            }
            (tops, bottoms)
        };
        // What is in view, where the cards held still will be once what
        // changed above them is followed.
        let mut top = -handle.offset().y;
        if let Some((anchor, was)) = self.steady[key].anchor()
            && let Some(now) = entries.iter().zip(&tops).find_map(|(entry, top)| match entry {
                ColumnEntry::Card { ix, .. }
                    if self.sections[section_ix(section)][*ix].name == anchor.as_ref() =>
                {
                    Some(*top)
                }
                _ => None,
            })
        {
            top += now - was;
        }
        let height = match handle.bounds().size.height {
            height if height > px(0.) => height,
            _ => window.viewport_size().height,
        };
        let (from, to) = (top - CARD_OVERDRAW, top + height + CARD_OVERDRAW);

        let mut drawn: Vec<Spaced> = Vec::new();
        let mut drawn_names = std::collections::HashSet::new();
        let mut composer_drawn = None;
        let mut unmeasured: Vec<(usize, bool)> = Vec::new();
        // Cards out of view, from where the first starts to where the last
        // ends.
        let mut skipped: Option<(Pixels, Pixels)> = None;
        let flush = |drawn: &mut Vec<Spaced>, skipped: &mut Option<(Pixels, Pixels)>| {
            if let Some((start, end)) = skipped.take() {
                drawn.push((div().w_full().flex_none().h(end - start).into_any_element(), 1.));
            }
        };
        for (at, entry) in entries.into_iter().enumerate() {
            match entry {
                ColumnEntry::Card { ix, open } => {
                    let name = name_of(self, ix);
                    let in_view = bottoms[at] >= from && tops[at] <= to;
                    let shown = in_view
                        || open
                        || focused.as_deref() == Some(name.as_str())
                        || settling.as_deref() == Some(name.as_str());
                    if !shown {
                        if !column.borrow().exact(&name) && unmeasured.len() < CARD_MEASURE_MOST {
                            unmeasured.push((ix, open));
                        }
                        let (start, _) = skipped.unwrap_or((tops[at], tops[at]));
                        skipped = Some((start, bottoms[at]));
                        continue;
                    }
                    flush(&mut drawn, &mut skipped);
                    let row = self.render_row(section, ix, open, window, cx);
                    let row = measured(row, column.clone(), name.clone());
                    // Moved any other way, it slides to its new place;
                    // settling from a drop, its copy brings it there instead.
                    let row = if settling.as_deref() == Some(name.as_str()) {
                        row
                    } else {
                        self.slides.slide(name.clone(), scrolls.clone(), row).into_any_element()
                    };
                    let row = self.steady[key].track(name.clone(), row).into_any_element();
                    drawn.push((row, 1.));
                    drawn_names.insert(name);
                }
                ColumnEntry::Drawn(element, there, key, _) => {
                    flush(&mut drawn, &mut skipped);
                    if composer_at == Some(at) {
                        composer_drawn = Some(drawn.len());
                    }
                    let element = match key {
                        Some(key) => measured(element, column.clone(), key.to_string()),
                        None => element,
                    };
                    drawn.push((element, there));
                }
            }
        }
        flush(&mut drawn, &mut skipped);

        // A card no longer drawn has no place on screen to be found at, or
        // to slide from when it is drawn again.
        {
            let mut cards = column.borrow_mut();
            let mut bounds = self.row_bounds.borrow_mut();
            for name in cards.drawn.difference(&drawn_names) {
                bounds.remove(name);
                self.slides.forget(name);
            }
            cards.drawn = drawn_names;
        }

        // A few of the cards out of view not yet measured, measured as the
        // column lays them out, once it has been laid out this frame.
        if !unmeasured.is_empty() {
            let this = cx.entity().downgrade();
            let column = column.clone();
            drawn.push((
                canvas(
                    move |_, window, cx| {
                        let Some(width) = column.borrow().width else {
                            return;
                        };
                        let space = size(AvailableSpace::Definite(width), AvailableSpace::MinContent);
                        let deadline = Instant::now() + CARD_MEASURE_BUDGET;
                        let mut any = false;
                        for (ix, open) in unmeasured {
                            let measured = this.update(cx, |this, cx| {
                                let name = this.sections[section_ix(section)].get(ix)?.name.clone();
                                let mut row = this.render_row(section, ix, open, window, cx);
                                Some((name, row.layout_as_root(space, window, cx).height))
                            });
                            if let Ok(Some((name, height))) = measured {
                                column.borrow_mut().record(&name, height, width);
                                any = true;
                            }
                            if Instant::now() >= deadline {
                                break;
                            }
                        }
                        // The column is laid out again with what was learnt,
                        // and the rest measured.
                        if any {
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_0()
                .into_any_element(),
                0.,
            ));
        }
        (drawn, composer_drawn)
    }

    /// A list's "Add story" line, as the PlanningScope's layout draws it.
    fn render_add_line(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let palette = crate::theme::palette(cx);
        let hover = crate::theme::color(palette.hover);
        let accent = crate::theme::color(palette.accent);
        let name = add_line(section);
        let focus = self.row_focus(&name, cx);
        let focused = focus.is_focused(window);
        let (muted, foreground) = (theme.muted_foreground, theme.foreground);
        let line = h_flex()
            .id(SharedString::from(format!("planning-add-story-{}", section.key())))
            .key_context(ROW_CONTEXT)
            .track_focus(&focus)
            .relative()
            .w_full()
            .flex_none()
            .h(px(32.))
            .px(px(10.))
            .gap_2()
            .items_center()
            .rounded(px(6.))
            .cursor_pointer()
            .text_sm()
            .text_color(muted)
            .aria_label("Add story")
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(format!("Add a story here ({MOD}N)"))
                    .build(window, cx)
            })
            .hover(move |line| line.bg(hover).text_color(foreground))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.compose_at_foot(section, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &PreviousRow, window, cx| this.step_rows(-1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &NextRow, window, cx| this.step_rows(1, window, cx)))
            .on_action(
                cx.listener(|this, _: &PreviousColumn, window, cx| {
                    this.step_columns(-1, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &NextColumn, window, cx| this.step_columns(1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleStory, window, cx| this.enter_row(window, cx)))
            .child(record_bounds(self.row_bounds.clone(), name))
            .child(Icon::new(IconName::Plus).xsmall())
            .child("Add story")
            .when(focused, |line| {
                line.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .rounded(px(6.))
                        .border_2()
                        .border_color(accent),
                )
            });
        gpui_kit::TestSupportExt::test_support(line).into_any_element()
    }

    /// The composer: a story card, its title a text box, with the focus
    /// ring around it, and beneath it how to add or cancel.
    fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(composer) = &self.composer else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let palette = crate::theme::palette(cx);
        let raised = crate::theme::color(palette.raised);
        let accent = crate::theme::color(palette.accent);
        let tertiary = crate::theme::color(palette.text_tertiary);
        let card = v_flex()
            .id(SharedString::from(format!(
                "planning-composer-{}",
                composer.section.key()
            )))
            .relative()
            .w_full()
            .flex_none()
            .py_2()
            .px(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(crate::theme::color(palette.line))
            .bg(raised)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(GRIP_WIDTH)
                            .h(px(20.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::GripVertical)
                                    .xsmall()
                                    .text_color(tertiary),
                            ),
                    )
                    .child(
                        div().flex_1().min_w_0().text_sm().child(
                            Input::new(&composer.title).appearance(false).small(),
                        ),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .mt_1()
                    .h(PROPERTY_HEIGHT)
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(mode_pill_label(composer.mode, cx)),
            )
            // The focus ring around the card while it is written.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .rounded(px(6.))
                    .border_2()
                    .border_color(accent),
            );
        gpui_kit::TestSupportExt::test_support(
            v_flex()
                .w_full()
                .flex_none()
                .gap_1()
                .child(card)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Enter to add, Escape to cancel"),
                ),
        )
        .into_any_element()
    }

    /// The mode picker of the story named so, beneath its pill, or, with
    /// none, of the bulk bar, above it, setting every selected story's mode;
    /// `mode` checked, and `highlighted` highlighted.
    fn render_mode_menu(
        &self,
        name: Option<&str>,
        mode: Option<SendMode>,
        highlighted: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let id = name.unwrap_or("bulk").to_string();
        let items = SendMode::ALL
            .into_iter()
            .enumerate()
            .map(|(at, choice)| {
                let name = name.map(str::to_string);
                h_flex()
                    .id(SharedString::from(format!(
                        "planning-mode-{id}-{}",
                        mode_label(choice)
                    )))
                    .px_2()
                    .h(px(26.))
                    .gap_2()
                    .items_center()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .when(at == highlighted, |item| item.bg(theme.list_active))
                    .hover(|item| item.bg(theme.list_hover))
                    .aria_label(mode_label(choice))
                    .on_click(cx.listener(move |this, _, _, cx| match &name {
                        Some(name) => this.set_mode(name, choice, cx),
                        None => this.set_selected_mode(choice, cx),
                    }))
                    .child(
                        div().w(px(14.)).children(
                            (Some(choice) == mode).then(|| Icon::new(IconName::Check).xsmall()),
                        ),
                    )
                    .child(mode_pill_label(choice, cx))
            })
            .collect::<Vec<_>>();
        let bulk = name.is_none();
        let menu = v_flex()
            .id(SharedString::from(format!("planning-mode-menu-{id}")))
            .when(!bulk, |menu| menu.mt(px(26.)))
            .when(bulk, |menu| menu.mb(px(4.)))
            .w(px(140.))
            .p_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_md()
            .occlude()
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                if bulk {
                    this.bulk_mode_menu = None;
                } else {
                    this.mode_menu = None;
                }
                cx.notify();
            }))
            .children(items);
        deferred(
            anchored()
                .when(bulk, |menu| menu.anchor(gpui_kit::Anchor::BottomLeft))
                .snap_to_window()
                .child(gpui_kit::TestSupportExt::test_support(menu)),
        )
        .with_priority(2)
    }

    fn render_row(
        &mut self,
        section: Section,
        ix: usize,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let story = &self.sections[section_ix(section)][ix];
        let name = story.name.clone();
        let mode = story.mode();
        let status = self.status(story, cx);
        let (from_agent, updated, attached) =
            (story.from_agent, story.updated, story.has_attachments());
        let drafted = story.drafted;
        // The whole list, its own prompt step 1, for the tooltip.
        let text = story.text();
        let steps: Vec<String> = std::iter::once(text.as_str())
            .chain(story.steps().iter().map(|step| step.prompt.as_str()))
            .enumerate()
            .map(|(at, prompt)| {
                format!("{}. {}", at + 1, prompt.lines().next().unwrap_or_default())
            })
            .collect();
        let spec_lines: Vec<String> = story
            .spec
            .iter()
            .map(|citation| match &citation.note {
                note if note.is_empty() => format!("- {}", citation.label()),
                note => format!("- {}: {note}", citation.label()),
            })
            .collect();
        let task = story.task.clone();
        let untitled = story.title.trim().is_empty();
        let title: SharedString = if untitled {
            "Untitled".into()
        } else {
            story.title.clone().into()
        };
        let focus = self.row_focus(&name, cx);
        let focused = focus.is_focused(window);
        let group = SharedString::from(format!("planning-row-{name}"));
        let paused = status.is_some_and(|(state, _)| state == StoryState::Paused);
        let confirming = self.confirm_delete.as_deref() == Some(name.as_str());
        let current = section == Section::Current;
        // Its own draft with the agent: going, failed, or what it left.
        let draft = self.story_drafts.get(&name);
        let drafting = draft.is_some_and(|draft| draft.running.is_some());
        let draft_error = draft.and_then(|draft| draft.error.clone());
        let draft_details = draft.is_some_and(|draft| draft.details_open);
        let proposed_nothing = draft.is_some_and(|draft| draft.proposed_nothing);
        let waiting = self.waiting(&name).len();
        let in_lists = LISTS.contains(&section);
        let draft_item = |names: Vec<String>| match (in_lists, drafting) {
            (false, _) => DraftItem::None,
            (true, true) if names.len() < 2 => DraftItem::Stop,
            (true, _) => DraftItem::Draft(names),
        };
        let hover_item = draft_item(vec![name.clone()]);
        // Right-clicked in a selection of several, it drafts them all.
        let context_item = draft_item(
            if self.selected.contains(&name) && self.selected.len() > 1 {
                self.selected_in_order()
            } else {
                vec![name.clone()]
            },
        );

        // The mode picker: its pill, and beneath it, while open, the modes.
        let mode_menu = self
            .mode_menu
            .as_ref()
            .filter(|(of, _)| *of == name)
            .map(|(_, at)| *at)
            .map(|highlighted| self.render_mode_menu(Some(&name), Some(mode), highlighted, cx));
        let mode_picker = {
            let name = name.clone();
            Button::new(SharedString::from(format!("planning-mode-{name}")))
                .ghost()
                .xsmall()
                .child(mode_pill_label(mode, cx))
                .disabled(current)
                .tooltip("How it will be sent (M)")
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.mode_menu.as_ref().is_some_and(|(of, _)| *of == name) {
                        this.mode_menu = None;
                        cx.notify();
                    } else {
                        this.open_mode_menu(&name, cx);
                    }
                }))
        };
        let resume = paused.then(|| {
            let name = name.clone();
            Button::new(SharedString::from(format!("planning-resume-{name}")))
                .ghost()
                .xsmall()
                .icon(IconName::Play)
                .label("Resume")
                .tooltip("Resume this story's task where it left off")
                .on_click(cx.listener(move |this, _, window, cx| this.resume(&name, window, cx)))
        });
        let this = cx.entity().downgrade();
        let sent = task.is_some();
        let menu = {
            let (this, name) = (this.clone(), name.clone());
            Button::new(SharedString::from(format!("planning-menu-{name}")))
                .ghost()
                .xsmall()
                .icon(IconName::Ellipsis)
                .label("More")
                .tooltip("More")
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
                    story_menu(menu, &this, &name, section, sent, hover_item.clone())
                })
        };
        let confirm = confirming.then(|| {
            h_flex()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.danger)
                        .child("Delete this story?"),
                )
                .child(
                    Button::new(SharedString::from(format!("planning-delete-{name}")))
                        .danger()
                        .xsmall()
                        .label("Delete")
                        .tooltip("Delete it for good (Enter)")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.confirm_delete(window, cx)),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!("planning-keep-{name}")))
                        .ghost()
                        .xsmall()
                        .label("Cancel")
                        .tooltip("Keep it (Escape)")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_delete = None;
                            cx.notify();
                        })),
                )
        });

        // Picked up, a full-size copy of it, as big as it was last drawn;
        // selected, it carries the whole selection, in its order.
        let selected = self.selected.contains(&name);
        let carried: Vec<String> = if selected {
            self.selected_in_order()
                .into_iter()
                .filter(|other| *other != name)
                .collect()
        } else {
            Vec::new()
        };
        let drag = self.card_face(&name).map(|mut face| StoryDrag {
            name: name.clone(),
            face: {
                face.count = carried.len() + 1;
                face
            },
            carried,
            size: self.row_bounds.borrow().get(&name).map_or(
                size(COLUMN_WIDTH - LIST_PADDING * 2., px(64.)),
                |bounds| bounds.size,
            ),
            grab: Rc::default(),
        });
        // Its copy settling into its place stands in for it until it has.
        let settling = self
            .settle
            .as_ref()
            .is_some_and(|settle| settle.name == name);
        let click_name = name.clone();
        let steps_tip: SharedString = steps.join("\n").into();
        let spec_tip: SharedString = if spec_lines.is_empty() {
            SharedString::default()
        } else {
            format!("Spec:\n{}", spec_lines.join("\n")).into()
        };
        // Every step, its own prompt among them.
        let step_count = steps.len();
        // While the side panel shows it, or drafts about it.
        let highlighted = open
            || (self.open.is_none()
                && (self.drafting.is_some() || !self.proposals.is_empty())
                && self.panel_about().as_deref() == Some(name.as_str()));
        // Only Draft, the Backlog, and Next sprint take drops, and only their
        // cards can be picked up.
        let takes_drops = matches!(section, Section::Draft | Section::Backlog | Section::Next);
        // A sent story's steps on its card: in Current sprint always, in
        // Done collapsed to their count until expanded.
        let sent_steps = match section {
            Section::Current => self.render_story_steps(&name, None, cx),
            Section::Done => self.render_story_steps(&name, Some(format!("done:{name}")), cx),
            _ => None,
        };
        let palette = crate::theme::palette(cx);
        let raised = crate::theme::color(palette.raised);
        let hover = crate::theme::color(palette.hover);
        let selected_bg = crate::theme::color(palette.selected);
        let line = crate::theme::color(palette.line);
        let accent = crate::theme::color(palette.accent);
        let tertiary = crate::theme::color(palette.text_tertiary);
        let status_shown = self.status_shown(status, current, cx);
        let card = v_flex()
            .id(SharedString::from(format!("planning-story-{name}")))
            .key_context(ROW_CONTEXT)
            .track_focus(&focus)
            .aria_label(title.clone())
            .group(group.clone())
            .relative()
            .w_full()
            .flex_none()
            .py_2()
            .px(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(line)
            .cursor_pointer()
            // Selected, the selected surface, hovered or not; else hovered,
            // one surface step further raised.
            .when(selected, |card| card.bg(selected_bg))
            .when(!selected, |card| card.bg(raised).hover(move |card| card.bg(hover)))
            // Done, muted.
            .when(section == Section::Done, |card| card.opacity(0.6))
            .on_hover(cx.listener({
                let name = name.clone();
                move |this, hovered: &bool, _, _| {
                    if *hovered {
                        this.hovered = Some(name.clone());
                    } else if this.hovered.as_deref() == Some(name.as_str()) {
                        this.hovered = None;
                    }
                }
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                // Ctrl/Cmd+click toggles it in or out of the selection, and
                // Shift+click selects the range to it, in the lists that
                // take moves; a plain click leaves the selection as it is.
                let modifiers = event.modifiers();
                if takes_drops && (modifiers.secondary() || modifiers.shift) {
                    this.focus_row(&click_name, window, cx);
                    if modifiers.shift {
                        this.select_range(&click_name, cx);
                    } else {
                        this.toggle_selected(&click_name, cx);
                    }
                    return;
                }
                if takes_drops {
                    this.select_from.insert(section.key(), click_name.clone());
                }
                // A story sent opens its task; any other opens, or closes.
                this.focus_row(&click_name, window, cx);
                if let Some(task) = task.as_deref().filter(|_| !open) {
                    (this.hooks.open_task)(task, window, cx);
                } else if open {
                    this.close_story(window, cx);
                } else {
                    this.show_story(&click_name, window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &PreviousRow, window, cx| this.step_rows(-1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &NextRow, window, cx| this.step_rows(1, window, cx)))
            .on_action(
                cx.listener(|this, _: &PreviousColumn, window, cx| {
                    this.step_columns(-1, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &NextColumn, window, cx| this.step_columns(1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleStory, window, cx| this.enter_row(window, cx)))
            .on_action(cx.listener({
                let name = name.clone();
                move |this, _: &MoveStoryUp, window, cx| {
                    this.shift_story(&name, true, cx);
                    this.focus_row(&name, window, cx);
                }
            }))
            .on_action(cx.listener({
                let name = name.clone();
                move |this, _: &MoveStoryDown, window, cx| {
                    this.shift_story(&name, false, cx);
                    this.focus_row(&name, window, cx);
                }
            }))
            .on_action(cx.listener({
                let name = name.clone();
                move |this, _: &MoveStoryLeft, window, cx| {
                    this.shift_story_across(&name, false, cx);
                    this.focus_row(&name, window, cx);
                }
            }))
            .on_action(cx.listener({
                let name = name.clone();
                move |this, _: &MoveStoryRight, window, cx| {
                    this.shift_story_across(&name, true, cx);
                    this.focus_row(&name, window, cx);
                }
            }))
            .on_action(cx.listener({
                let name = name.clone();
                move |this, _: &PickMode, _, cx| this.open_mode_menu(&name, cx)
            }))
            .on_action(cx.listener({
                let name = name.clone();
                move |this, _: &ToggleSprint, window, cx| this.toggle_sprint(&name, window, cx)
            }))
            .on_action(cx.listener({
                let name = name.clone();
                move |this, _: &DeleteStory, _, cx| this.ask_delete(&name, cx)
            }))
            .on_action(cx.listener({
                let name = name.clone();
                move |this, _: &ToggleSelected, _, cx| this.toggle_selected(&name, cx)
            }))
            .when_some(drag.filter(|_| takes_drops), |card, drag| {
                card.on_drag(drag, |drag, grab, _, cx| {
                    drag.grab.set(grab);
                    cx.new(|_| drag.clone())
                })
            })
            .when(settling, |card| card.invisible())
            .child(record_bounds(self.row_bounds.clone(), name.clone()))
            // The story the side panel shows keeps its line in the accent
            // along its left edge, hovered or not.
            .when(highlighted, |card| {
                card.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .w(px(3.))
                        .rounded_tl(px(6.))
                        .rounded_bl(px(6.))
                        .bg(accent),
                )
            })
            // Its top line: the grip, its title, the card's one primary
            // thing, and its menu at its right.
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_start()
                    .child(
                        own_clicks(
                            div()
                                .w(GRIP_WIDTH)
                                .h(px(20.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center(),
                        )
                        .child(
                            Icon::new(IconName::GripVertical)
                                .xsmall()
                                .text_color(tertiary)
                                .when(takes_drops, |grip| grip.cursor_grab()),
                        ),
                    )
                    // Wrapping onto a second line before it is cut off.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .line_clamp(2)
                            .text_ellipsis()
                            .text_color(if untitled { muted } else { theme.foreground })
                            .child(title),
                    )
                    .when(!confirming && !current, |line| {
                        line.child(
                            own_clicks(h_flex())
                                .flex_none()
                                .when(!focused, |buttons| {
                                    buttons
                                        .invisible()
                                        .group_hover(group, |this| this.visible())
                                })
                                .child(menu),
                        )
                    }),
            )
            // Beneath the title, its one muted property row.
            .child(
                h_flex()
                    .w_full()
                    .mt_1()
                    .h(PROPERTY_HEIGHT)
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    // The mode pill, taking its own clicks.
                    .child(
                        own_clicks(div().relative().flex_none())
                            .child(mode_picker)
                            .children(mode_menu),
                    )
                    // From the agent, until it is first opened.
                    .when(from_agent, |meta| {
                        meta.child(
                            div()
                                .id(SharedString::from(format!("planning-from-agent-{name}")))
                                .flex_none()
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new("From agent")
                                        .build(window, cx)
                                })
                                .child(
                                    Icon::new(IconName::Sparkles)
                                        .xsmall()
                                        .text_color(theme.primary),
                                ),
                        )
                    })
                    .when(step_count > 1, |meta| {
                        meta.child(
                            div()
                                .id(SharedString::from(format!("planning-steps-{name}")))
                                .flex_none()
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(steps_tip.clone())
                                        .build(window, cx)
                                })
                                .child(format!("{step_count} steps")),
                        )
                    })
                    .when(attached, |meta| {
                        meta.child(Icon::new(IconName::Paperclip).xsmall().text_color(muted))
                    })
                    .when(!spec_tip.is_empty(), |meta| {
                        meta.child(
                            div()
                                .id(SharedString::from(format!("planning-spec-{name}")))
                                .flex_none()
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(spec_tip.clone())
                                        .build(window, cx)
                                })
                                .child(Icon::new(IconName::BookOpen).xsmall().text_color(muted)),
                        )
                    })
                    .when(updated, |meta| {
                        meta.child(
                            div()
                                .flex_none()
                                .text_color(theme.primary)
                                .child("Updated by agent"),
                        )
                    })
                    // Drafted in Draft, its stories created, with any of its
                    // own draft's proposals still waiting after it; or those
                    // waiting, or that it proposed nothing.
                    .when(!drafting && drafted > 0, |meta| {
                        meta.child(
                            div()
                                .id(SharedString::from(format!("planning-drafted-{name}")))
                                .flex_none()
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(match drafted {
                                        1 => "1 story added to the Backlog".to_string(),
                                        n => format!("{n} stories added to the Backlog"),
                                    })
                                    .build(window, cx)
                                })
                                .child("Drafted"),
                        )
                        .when(waiting > 0, |meta| meta.child(div().flex_none().child("·")))
                    })
                    .when(!drafting && waiting > 0, |meta| {
                        meta.child(div().flex_none().text_color(accent).child(match waiting {
                            1 => "1 proposal".to_string(),
                            n => format!("{n} proposals"),
                        }))
                    })
                    .when(!drafting && waiting == 0 && proposed_nothing, |meta| {
                        meta.child(div().flex_none().child("Nothing proposed"))
                    })
                    .child(div().flex_1())
                    // Drafting with the agent, in place of its status; else,
                    // once sent, its status and time at the row's right.
                    .when(drafting, |meta| {
                        meta.child(
                            h_flex()
                                .id(SharedString::from(format!("planning-drafting-{name}")))
                                .flex_none()
                                .gap_1()
                                .items_center()
                                .child(Spinner::new().xsmall())
                                .child("Drafting…"),
                        )
                    })
                    .when(!drafting, |meta| {
                        meta.child(
                            div()
                                .flex_none()
                                .flex()
                                .justify_end()
                                .children(status_shown),
                        )
                    })
                    .children(resume.map(|resume| own_clicks(div().flex_none()).child(resume))),
            )
            .children(draft_error.map(|error| {
                own_clicks(div().w_full()).child(self.render_draft_error(
                    &name,
                    error,
                    draft_details,
                    cx,
                ))
            }))
            .children(sent_steps)
            .children(confirm.map(|confirm| own_clicks(div().w_full()).child(confirm)))
            // Focus: the accent ring, over the open story's line and any
            // hover.
            .when(focused, |card| {
                card.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .rounded(px(6.))
                        .border_2()
                        .border_color(accent),
                )
            });
        // Right-clicked, the same menu its button opens, at the pointer.
        let menu_name = name.clone();
        let card = div()
            .id(SharedString::from(format!("planning-story-menu-{name}")))
            .w_full()
            .flex_none()
            .child(gpui_kit::TestSupportExt::test_support(card));
        if current {
            return card.into_any_element();
        }
        card.context_menu(move |menu, _, _| {
            story_menu(menu, &this, &menu_name, section, sent, context_item.clone())
        })
        .into_any_element()
    }

    fn render_open(
        &self,
        name: &str,
        proposals: Option<AnyElement>,
        findings: Option<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (Some(story), Some(open)) = (self.story(name), self.open.as_ref()) else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let name = story.name.clone();
        let agent = crate::agent::of_project(Some(&self.project_dir));
        let (height, _) = open.fit.heights(&open.prompt, window, cx);
        let prompt = div()
            .relative()
            .w_full()
            .child(Editor::new(&open.prompt).h(height))
            .child(GrowToFit::tracker(
                &open.prompt,
                cx.entity().downgrade(),
                |this: &mut Self| &mut this.open.as_mut().expect("a story is open").fit,
            ))
            .child(open.completion.clone());
        let steps = self.render_steps(story, prompt.into_any_element(), open.highlight, window, cx);
        let spec = v_flex()
            .gap_1()
            .child(div().text_xs().text_color(muted).child("Spec"))
            .child(self.render_citations(&name, &story.spec, Some(&name), cx))
            .when(story.task.is_none(), |spec| {
                // The spec's elements matching what is typed, to pick from.
                let query = open.spec_input.read(cx).value().to_string();
                let picks = citation_picks(&open.spec_elements, &query)
                    .into_iter()
                    .enumerate()
                    .map(|(at, (link, element))| {
                        let (name, link) = (name.clone(), link.clone());
                        let file = link.split('#').next().unwrap_or_default().to_string();
                        let input = open.spec_input.clone();
                        gpui_kit::TestSupportExt::test_support(
                            h_flex()
                                .id(SharedString::from(format!("planning-cite-pick-{at}")))
                                .gap_2()
                                .px_2()
                                .h(px(24.))
                                .items_center()
                                .rounded(theme.radius)
                                .cursor_pointer()
                                .text_xs()
                                .hover(|row| row.bg(theme.list_hover))
                                .aria_label(element.clone())
                                .child(div().flex_none().child(element.clone()))
                                .child(div().min_w_0().truncate().text_color(muted).child(file))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if this.add_citation(&name, &link, cx) {
                                        input.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                    }
                                })),
                        )
                    })
                    .collect::<Vec<_>>();
                spec.child(Input::new(&open.spec_input).small())
                    .child(v_flex().gap_0p5().children(picks))
            })
            .children(
                open.spec_error
                    .clone()
                    .map(|why| div().text_xs().text_color(theme.danger).child(why)),
            );
        let attachments = {
            let mut items: Vec<(String, usize, u8)> = Vec::new();
            for (ix, text) in story.anchor.attached_text.iter().enumerate() {
                let first = text.lines().next().unwrap_or_default();
                items.push((format!("Text: {first}"), ix, 0));
            }
            for (ix, path) in story.anchor.attached_images.iter().enumerate() {
                items.push((file_name(path), ix, 1));
            }
            for (ix, path) in story.anchor.attached_files.iter().enumerate() {
                items.push((file_name(path), ix, 2));
            }
            h_flex()
                .flex_wrap()
                .gap_1()
                .children(items.into_iter().map(|(label, ix, kind)| {
                    let name = name.clone();
                    h_flex()
                        .gap_1()
                        .pl_2()
                        .rounded(px(4.))
                        .bg(theme.secondary)
                        .text_xs()
                        .child(Icon::new(IconName::Paperclip).xsmall().text_color(muted))
                        .child(div().max_w(px(220.)).truncate().child(label.clone()))
                        .child(
                            Button::new(SharedString::from(format!(
                                "planning-detach-{name}-{kind}-{ix}"
                            )))
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .label("Remove")
                            .tooltip(format!("Remove {label}"))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.change(
                                        &name,
                                        |anchor| match kind {
                                            0 => {
                                                anchor.attached_text.remove(ix);
                                            }
                                            1 => {
                                                anchor.attached_images.remove(ix);
                                            }
                                            _ => {
                                                anchor.attached_files.remove(ix);
                                            }
                                        },
                                        cx,
                                    )
                                },
                            )),
                        )
                }))
                .child({
                    let name = name.clone();
                    Button::new(SharedString::from(format!("planning-attach-{name}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Paperclip)
                        .label("Attach")
                        .tooltip("Attach files to this story")
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.attach(&name, window, cx)),
                        )
                })
        };
        // Its parameters, as the chat input shows them.
        let this = cx.entity().downgrade();
        let model = story.anchor.model.clone();
        let model_picker = {
            let (this, name) = (this.clone(), name.clone());
            Button::new(SharedString::from(format!("planning-model-{name}")))
                .ghost()
                .xsmall()
                .label(crate::models::label(agent, model.as_deref()))
                .tooltip("The model it is sent to")
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |mut menu, _, _| {
                    let choices = std::iter::once((None, "Default".to_string())).chain(
                        crate::models::available(agent)
                            .into_iter()
                            .map(|model| (Some(model.id), model.label)),
                    );
                    for (id, label) in choices {
                        let (this, name) = (this.clone(), name.clone());
                        let checked = id == model;
                        menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                            move |_, _, cx| {
                                let id = id.clone();
                                this.update(cx, |this, cx| {
                                    this.change(&name, |anchor| anchor.model = id, cx)
                                })
                                .ok();
                            },
                        ));
                    }
                    menu
                })
        };
        let efforts = crate::effort::available(agent);
        let effort = story.anchor.effort.clone();
        let effort_picker = (!efforts.is_empty()).then(|| {
            let (this, name) = (this.clone(), name.clone());
            Button::new(SharedString::from(format!("planning-effort-{name}")))
                .ghost()
                .xsmall()
                .label(crate::effort::label(agent, effort.as_deref()))
                .tooltip("The reasoning effort it is sent with")
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |mut menu, _, _| {
                    let choices = std::iter::once((None, "Default".to_string())).chain(
                        efforts
                            .iter()
                            .map(|effort| (Some(effort.id.clone()), effort.label.clone())),
                    );
                    for (id, label) in choices {
                        let (this, name) = (this.clone(), name.clone());
                        let checked = id == effort;
                        menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                            move |_, _, cx| {
                                let id = id.clone();
                                this.update(cx, |this, cx| {
                                    this.change(&name, |anchor| anchor.effort = id, cx)
                                })
                                .ok();
                            },
                        ));
                    }
                    menu
                })
        });
        let sliced = story.anchor.sliced;
        let slice = {
            let name = name.clone();
            Button::new(SharedString::from(format!("planning-slice-{name}")))
                .ghost()
                .xsmall()
                .when(sliced, |button| button.icon(IconName::Check))
                .label("Slice")
                .tooltip("Send the slice of each spec it references, rather than links to them")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.change(&name, |anchor| anchor.sliced = !anchor.sliced, cx)
                }))
        };
        let ask_agent = (story.task.is_none()).then(|| {
            let name = name.clone();
            Button::new(SharedString::from(format!("planning-ask-agent-{name}")))
                .ghost()
                .xsmall()
                .icon(IconName::Sparkles)
                .label("Ask agent")
                .tooltip("Have the agent propose what this story still needs")
                .on_click(cx.listener(move |this, _, window, cx| this.ask_agent(&name, window, cx)))
        });
        let label = |text: &'static str| div().text_xs().text_color(muted).child(text);
        let close = {
            let name = name.clone();
            Button::new(SharedString::from(format!("planning-close-{name}")))
                .ghost()
                .xsmall()
                .label("Close")
                .tooltip("Close this story (Escape)")
                .on_click(
                    cx.listener(move |this, _, window, cx| this.toggle_story(&name, window, cx)),
                )
                .into_any_element()
        };
        let heading_name: SharedString = if story.title.trim().is_empty() {
            "Untitled".into()
        } else {
            story.title.clone().into()
        };
        let content = v_flex()
            .gap_2()
            .child(label("Title"))
            .child(Input::new(&open.title))
            .child(steps)
            .child(attachments)
            // Its parameters, as the chat input shows them.
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(model_picker)
                    .children(effort_picker)
                    .child(slice),
            )
            .child(spec)
            .children(findings)
            .children(proposals);
        // The footer: right-aligned, the primary action last.
        let footer = h_flex().gap_1().justify_end().children(ask_agent);
        let editor = self.panel_box(
            format!("planning-open-{name}"),
            panel_heading(heading_name, Some(close)),
            content,
            footer,
            cx,
        );
        gpui_kit::TestSupportExt::test_support(editor).into_any_element()
    }

    /// The open story's steps, one list: its own `prompt` step 1, then
    /// each further step a row numbered from 2, its mode pill and first
    /// line, opening in place to edit, dragged or moved with Alt+Up and
    /// Alt+Down, and removed from its row or its menu; then Add step, and why a step couldn't be added. A sent
    /// story's steps read as they were sent.
    fn render_steps(
        &self,
        story: &Story,
        prompt: AnyElement,
        highlight: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let name = story.name.clone();
        let sent = story.task.is_some();
        let count = story.steps().len();
        let dragging = cx.has_active_drag();
        let gap = self.step_gap.get().filter(|_| dragging);
        // Its own prompt is step 1, a row of the list like the others, its
        // editor always open.
        let mut list = v_flex()
            .w_full()
            .gap(ROW_GAP)
            .child(div().text_xs().text_color(muted).child("Steps"))
            .child(
                h_flex()
                    .id(SharedString::from(format!("planning-step-{name}-prompt")))
                    .aria_label("Step 1")
                    .w_full()
                    .gap_2()
                    .px_2()
                    .items_start()
                    .child(
                        div()
                            .w(px(16.))
                            .min_h(px(28.))
                            .flex()
                            .items_center()
                            .text_xs()
                            .text_color(muted)
                            .child("1"),
                    )
                    .child(
                        div()
                            .flex_none()
                            .min_h(px(28.))
                            .flex()
                            .items_center()
                            .child(mode_pill_label(story.mode(), cx)),
                    )
                    .child(div().flex_1().min_w_0().child(prompt)),
            );
        for (at, step) in story.steps().iter().enumerate() {
            let focus = self
                .step_focus
                .get(&(name.clone(), at))
                .cloned()
                .unwrap_or_else(|| self.focus.clone());
            let focused = focus.is_focused(window);
            let highlighted = highlight == Some(at);
            let editing = self
                .open_step
                .as_ref()
                .filter(|open| open.story == name && open.at == at)
                .map(|open| open.prompt.clone());
            let first = step.prompt.lines().next().unwrap_or_default().to_string();
            let label: SharedString = format!("{}. {first}", at + 2).into();
            let mode_button = {
                let name = name.clone();
                Button::new(SharedString::from(format!(
                    "planning-step-mode-{name}-{at}"
                )))
                .ghost()
                .xsmall()
                .child(mode_pill_label(step.mode, cx))
                .disabled(sent)
                .tooltip("Code or Spec: click to switch")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.change(
                        &name,
                        |anchor| {
                            if let Some(step) = anchor.added_steps.get_mut(at) {
                                step.mode = if step.mode == SendMode::Spec {
                                    SendMode::Code
                                } else {
                                    SendMode::Spec
                                };
                            }
                        },
                        cx,
                    )
                }))
            };
            let remove = (!sent).then(|| {
                let name = name.clone();
                Button::new(SharedString::from(format!(
                    "planning-step-remove-{name}-{at}"
                )))
                .ghost()
                .xsmall()
                .icon(IconName::X)
                .tooltip(format!("Remove step {}", at + 2))
                .on_click(cx.listener(move |this, _, _, cx| this.remove_step(&name, at, cx)))
            });
            let menu = (!sent).then(|| {
                let this = cx.entity().downgrade();
                let name = name.clone();
                Button::new(SharedString::from(format!(
                    "planning-step-menu-{name}-{at}"
                )))
                .ghost()
                .xsmall()
                .icon(IconName::Ellipsis)
                .label("More")
                .tooltip("More")
                .dropdown_menu_with_anchor(
                    gpui_kit::Anchor::TopRight,
                    move |menu, _, _| {
                        let item = |label: &'static str,
                                    act: fn(
                            &mut Planning,
                            &str,
                            usize,
                            usize,
                            &mut Context<Planning>,
                        )| {
                            let (this, name) = (this.clone(), name.clone());
                            PopupMenuItem::new(label).on_click(move |_, _, cx| {
                                this.update(cx, |this, cx| act(this, &name, at, count, cx))
                                    .ok();
                            })
                        };
                        menu.item(item("Move up (Alt+Up)", |this, name, at, _, cx| {
                            if at > 0 {
                                this.move_step(name, at, at - 1, cx)
                            }
                        }))
                        .item(item("Move down (Alt+Down)", |this, name, at, count, cx| {
                            if at + 1 < count {
                                this.move_step(name, at, at + 1, cx)
                            }
                        }))
                        .separator()
                        .item(item("Remove step", |this, name, at, _, cx| {
                            this.remove_step(name, at, cx)
                        }))
                    },
                )
            });
            let drag = StepDrag {
                story: name.clone(),
                from: at,
                label: label.clone(),
            };
            let click_name = name.clone();
            let drop_name = name.clone();
            let row = h_flex()
                .id(SharedString::from(format!("planning-step-{name}-{at}")))
                .key_context(STEP_CONTEXT)
                .track_focus(&focus)
                .aria_label(label.clone())
                .relative()
                .gap_2()
                .px_2()
                .min_h(px(28.))
                .items_center()
                .rounded(px(4.))
                .border_1()
                .border_color(if focused {
                    theme.ring
                } else {
                    gpui_kit::transparent_black()
                })
                .cursor_pointer()
                .when(highlighted, |row| row.bg(theme.primary.opacity(0.12)))
                .when(!highlighted, |row| {
                    row.hover(|row| row.bg(theme.list_hover))
                })
                .when(!sent, |row| {
                    row.on_click(cx.listener(move |this, _, window, cx| {
                        this.open_step(&click_name, at, window, cx)
                    }))
                    .on_action(cx.listener(|this, _: &MoveStoryUp, window, cx| {
                        this.shift_step(true, window, cx)
                    }))
                    .on_action(cx.listener(|this, _: &MoveStoryDown, window, cx| {
                        this.shift_step(false, window, cx)
                    }))
                    .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                    .on_drag_move({
                        let gap = self.step_gap.clone();
                        move |event: &DragMoveEvent<StepDrag>, window, _| {
                            let bounds = event.bounds;
                            if !bounds.contains(&event.event.position) {
                                return;
                            }
                            let to = at + (event.event.position.y > bounds.center().y) as usize;
                            if gap.get() != Some(to) {
                                gap.set(Some(to));
                                window.refresh();
                            }
                        }
                    })
                    .on_drop(cx.listener(
                        move |this, drag: &StepDrag, _, cx| {
                            let gap = this.step_gap.take().unwrap_or(at);
                            if drag.story == drop_name {
                                // The gaps either side of it are where it is.
                                let to = if gap > drag.from { gap - 1 } else { gap };
                                this.move_step(&drop_name, drag.from, to, cx);
                            }
                        },
                    ))
                })
                .when(gap == Some(at), |row| {
                    row.child(crate::prompt_mode::drop_indicator(false, false, cx))
                })
                .when(gap == Some(at + 1) && at + 1 == count, |row| {
                    row.child(crate::prompt_mode::drop_indicator(false, true, cx))
                })
                .when(!sent, |row| {
                    row.child(
                        Icon::new(IconName::GripVertical)
                            .xsmall()
                            .text_color(muted)
                            .cursor_grab(),
                    )
                })
                .child(
                    div()
                        .w(px(16.))
                        .text_xs()
                        .text_color(muted)
                        .child(format!("{}", at + 2)),
                )
                .child(own_clicks(div().flex_none()).child(mode_button))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(if first.trim().is_empty() {
                            muted
                        } else {
                            theme.foreground
                        })
                        .child(if first.trim().is_empty() {
                            "Empty step".to_string()
                        } else {
                            first
                        }),
                )
                .when(highlighted, |row| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(theme.primary)
                            .child("Added by agent"),
                    )
                })
                .children(remove.map(|remove| own_clicks(div().flex_none()).child(remove)))
                .children(menu.map(|menu| own_clicks(div().flex_none()).child(menu)));
            list = list.child(gpui_kit::TestSupportExt::test_support(row));
            if let Some(prompt) = editing {
                list = list.child(div().pl(px(40.)).child(Textarea::new(&prompt)));
            }
        }
        if !sent {
            let name = name.clone();
            list = list.child(
                Button::new(SharedString::from(format!("planning-add-step-{name}")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .label("Add step")
                    .tooltip("Add a step after the last, done once the one before it is")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.add_step_here(&name, String::new(), window, cx)
                    })),
            );
        }
        if let Some((story, why, kept)) = &self.step_error
            && *story == name
        {
            list = list.child(
                v_flex()
                    .id(SharedString::from(format!("planning-step-error-{name}")))
                    .gap_0p5()
                    .child(div().text_xs().text_color(theme.danger).child(why.clone()))
                    .when(!kept.trim().is_empty(), |error| {
                        error.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("What was to be added: {kept}")),
                        )
                    }),
            );
        }
        list.into_any_element()
    }

    /// A proposal's card: where it goes, its title, mode, and prompt, and
    /// Accept, Edit, and Reject; edited in place while Edit has it open.
    fn render_proposal(
        &mut self,
        proposal: &Proposal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let id = proposal.id.clone();
        let focus = self.row_focus(&id, cx);
        let focused = focus.is_focused(window);
        let target = proposal.target.as_deref().and_then(|name| self.story(name));
        let (label, label_color) = match target {
            Some(story) => (
                if proposal.about.as_deref() == Some(story.name.as_str()) {
                    "Add to this story".to_string()
                } else {
                    format!("Add to {}", story.title)
                },
                chat_input::mode_color(story.mode(), cx),
            ),
            None => ("New story".to_string(), theme.accent),
        };
        // What it would add to: the story's own prompt and its steps.
        let target_steps = target
            .filter(|story| !story.steps().is_empty())
            .map(|story| {
                let mut lines = vec![format!(
                    "1. {}",
                    story.text().lines().next().unwrap_or_default()
                )];
                lines.extend(story.steps().iter().enumerate().map(|(at, step)| {
                    format!(
                        "{}. {}",
                        at + 2,
                        step.prompt.lines().next().unwrap_or_default()
                    )
                }));
                lines.join("\n")
            });
        let editing = self.editing.as_ref().filter(|editor| editor.id == id);
        let read_more = self.read_more.contains(&id);
        let (shown, cut) = if read_more {
            (proposal.prompt.clone(), false)
        } else {
            clipped(&proposal.prompt, PROPOSAL_LINES)
        };
        let refused = self.refused.get(&id).cloned();
        // A proposal citing nothing, not even No match, isn't a story yet.
        let ungrounded = proposal.spec.is_empty();
        let accept = {
            let id = id.clone();
            Button::new(SharedString::from(format!("planning-accept-{id}")))
                .primary()
                .xsmall()
                .icon(IconName::Check)
                .label("Accept")
                .tooltip("Record it in the plan (A)")
                .on_click(cx.listener(move |this, _, window, cx| {
                    if this.accept(&id, cx).is_ok() {
                        this.focus_after_decision(&id, window, cx);
                    }
                }))
        };
        let edit = {
            let id = id.clone();
            Button::new(SharedString::from(format!("planning-edit-{id}")))
                .ghost()
                .xsmall()
                .icon(IconName::Pencil)
                .label(if editing.is_some() { "Done" } else { "Edit" })
                .tooltip("Change it before it is accepted (E)")
                .on_click(cx.listener(move |this, _, window, cx| {
                    if this.editing.as_ref().is_some_and(|editor| editor.id == id) {
                        this.editing = None;
                        this.focus_row(&id, window, cx);
                    } else {
                        this.edit_proposal(&id, window, cx);
                    }
                }))
        };
        let reject = {
            let id = id.clone();
            Button::new(SharedString::from(format!("planning-reject-{id}")))
                .ghost()
                .xsmall()
                .icon(IconName::X)
                .label("Reject")
                .tooltip("Let it go (R)")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.focus_after_decision(&id, window, cx);
                    this.reject(&id, cx);
                }))
        };
        let card = card_box(SharedString::from(format!("planning-proposal-{id}")), cx)
            .key_context(PROPOSAL_CONTEXT)
            .track_focus(&focus)
            .aria_label(format!("Proposal: {}", proposal.story_title()))
            .when(focused, |card| card.border_color(theme.ring))
            .on_action(
                cx.listener(|this, _: &PreviousRow, window, cx| this.step_rows(-1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &NextRow, window, cx| this.step_rows(1, window, cx)))
            .on_action(
                cx.listener(|this, _: &PreviousColumn, window, cx| {
                    this.step_columns(-1, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &NextColumn, window, cx| this.step_columns(1, window, cx)),
            )
            .on_action(cx.listener({
                let id = id.clone();
                move |this, _: &AcceptProposal, window, cx| {
                    if this.accept(&id, cx).is_ok() {
                        this.focus_after_decision(&id, window, cx);
                    }
                }
            }))
            .on_action(cx.listener({
                let id = id.clone();
                move |this, _: &EditProposal, window, cx| this.edit_proposal(&id, window, cx)
            }))
            .on_action(cx.listener({
                let id = id.clone();
                move |this, _: &RejectProposal, window, cx| {
                    this.focus_after_decision(&id, window, cx);
                    this.reject(&id, cx);
                }
            }))
            .child(
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(label_color)
                    .child(label),
            )
            .children(
                proposal
                    .note
                    .clone()
                    .map(|note| div().text_xs().text_color(muted).child(note)),
            )
            .children(target_steps.map(|steps| {
                div()
                    .text_xs()
                    .text_color(muted)
                    .whitespace_normal()
                    .child(steps)
            }))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(mode_pill_label(proposal.mode(), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_medium()
                            .child(proposal.story_title()),
                    ),
            )
            .when(editing.is_none(), |card| {
                card.child(
                    div()
                        .text_sm()
                        .text_color(theme.foreground)
                        .whitespace_normal()
                        .child(shown),
                )
                .when(cut || read_more, |card| {
                    let id = id.clone();
                    card.child(
                        Button::new(SharedString::from(format!("planning-read-more-{id}")))
                            .ghost()
                            .xsmall()
                            .label(if read_more { "Show less" } else { "Read more" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.read_more.remove(&id) {
                                    this.read_more.insert(id.clone());
                                }
                                cx.notify();
                            })),
                    )
                })
            })
            // Its follow-up step, part of the story it follows.
            .children(proposal.step.as_ref().map(|step| {
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child("2.")
                    .child(mode_pill_label(step.mode(), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(step.prompt.lines().next().unwrap_or_default().to_string()),
                    )
            }))
            .child(self.render_citations(&id, &proposal.spec, None, cx))
            .children(
                proposal
                    .dropped
                    .iter()
                    .map(|why| div().text_xs().text_color(muted).child(why.clone())),
            )
            .children(refused.map(|why| div().text_xs().text_color(theme.danger).child(why)))
            .when(ungrounded, |card| {
                card.child(
                    div()
                        .text_xs()
                        .text_color(theme.warning)
                        .child("The drafter cited no spec, so this isn't offered as a story."),
                )
            })
            .child(
                h_flex()
                    .gap_1()
                    .justify_end()
                    .when(!ungrounded, |foot| foot.child(accept).child(edit))
                    .when(ungrounded, |foot| {
                        let id = id.clone();
                        foot.child(
                            Button::new(SharedString::from(format!("planning-draft-again-{id}")))
                                .primary()
                                .xsmall()
                                .icon(IconName::Sparkles)
                                .label("Draft again")
                                .tooltip(
                                    "Run the draft for this story once more, grounded in the spec",
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.draft_again(&id, window, cx)
                                })),
                        )
                    })
                    .child(reject),
            );
        // The editor beside the card rather than in it, so typing never
        // reaches the card's own keys.
        let editor = editing.map(|editor| {
            let (title, prompt) = (editor.title.clone(), editor.prompt.clone());
            let this = cx.entity().downgrade();
            let mode = proposal.mode();
            let mode_picker = {
                let (this, id) = (this.clone(), id.clone());
                Button::new(SharedString::from(format!("planning-proposal-mode-{id}")))
                    .ghost()
                    .xsmall()
                    .child(mode_pill_label(mode, cx))
                    .tooltip("How it will be sent")
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |mut menu, _, _| {
                        for choice in SendMode::ALL {
                            let (this, id) = (this.clone(), id.clone());
                            menu = menu.item(
                                PopupMenuItem::new(mode_label(choice))
                                    .checked(choice == mode)
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |this, cx| {
                                            this.set_proposal(
                                                &id,
                                                |proposal| proposal.mode = mode_slug(choice).into(),
                                                cx,
                                            )
                                        })
                                        .ok();
                                    }),
                            );
                        }
                        menu
                    })
            };
            let mut targets = self.targets(proposal.about.as_deref());
            // A draft about a story never adds to the story drafted.
            if proposal.drafted {
                targets.retain(|(name, _)| proposal.about.as_ref() != Some(name));
            }
            let current = proposal.target.clone();
            let target_picker = {
                let (this, id) = (this.clone(), id.clone());
                Button::new(SharedString::from(format!("planning-proposal-target-{id}")))
                    .ghost()
                    .xsmall()
                    .label(
                        current
                            .as_deref()
                            .and_then(|name| targets.iter().find(|(target, _)| target == name))
                            .map_or("New story".to_string(), |(name, title)| {
                                if proposal.about.as_ref() == Some(name) {
                                    "Add to this story".to_string()
                                } else {
                                    format!("Add to {title}")
                                }
                            }),
                    )
                    .tooltip("Where it goes")
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |mut menu, _, _| {
                        let choices = std::iter::once((None, "New story".to_string())).chain(
                            targets
                                .iter()
                                .map(|(name, title)| (Some(name.clone()), title.clone())),
                        );
                        for (target, label) in choices {
                            let (this, id) = (this.clone(), id.clone());
                            let checked = target == current;
                            menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                                move |_, _, cx| {
                                    let target = target.clone();
                                    this.update(cx, |this, cx| {
                                        this.set_proposal(
                                            &id,
                                            |proposal| proposal.target = target,
                                            cx,
                                        )
                                    })
                                    .ok();
                                },
                            ));
                        }
                        menu
                    })
            };
            let label = |text: &'static str| div().text_xs().text_color(muted).child(text);
            v_flex()
                .gap_2()
                .p_2()
                .rounded(theme.radius)
                .bg(theme.background)
                .child(label("Title"))
                .child(Input::new(&title))
                .child(label("Prompt"))
                .child(Textarea::new(&prompt))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(mode_picker)
                        .child(target_picker),
                )
        });
        gpui_kit::TestSupportExt::test_support(
            v_flex()
                .id(SharedString::from(format!("planning-proposal-block-{id}")))
                .w_full()
                .gap_1()
                .child(gpui_kit::TestSupportExt::test_support(card))
                .children(editor),
        )
        .into_any_element()
    }

    /// The Findings list, newest first: each a row with its topic, when it
    /// ran, and its state, opening its findings in a body tab, deleted from
    /// its menu; only those started from `story` where one is given. None
    /// while there are none.
    /// The story `name`'s steps, a row each, its own prompt step 1, then
    /// the rest in order, indented beneath it, each with its own status once
    /// the story is sent; none while it has no further step. Listed at `collapse`, they show only their count until it is
    /// expanded.
    fn render_story_steps(
        &self,
        name: &str,
        collapse: Option<String>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let story = self.story(name)?;
        if story.steps().is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let task = story.task.clone();
        let count = story.steps().len() + 1;
        let current = collapse.is_none();
        let missing = self.state(story, cx) == Some(StoryState::Missing);
        let expanded = collapse
            .as_ref()
            .is_none_or(|key| self.steps_expanded.contains(key));
        let toggle = collapse.map(|key| {
            let id = SharedString::from(format!("planning-step-list-{key}"));
            h_flex()
                .id(id)
                .h(px(24.))
                .gap_1()
                .items_center()
                .rounded(theme.radius)
                .cursor_pointer()
                .text_xs()
                .text_color(muted)
                .hover(|row| row.bg(theme.list_hover))
                .aria_label(if expanded { "Hide steps" } else { "Show steps" })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.steps_expanded.remove(&key) {
                        this.steps_expanded.insert(key.clone());
                    }
                    cx.notify();
                }))
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .xsmall(),
                )
                .child(format!("{count} steps"))
        });
        // The whole list: its own prompt step 1, as its story stands, then
        // each further step.
        let text = story.text();
        let own = (self.status(story, cx), text.as_str());
        let rows = std::iter::once(own)
            .chain(story.steps().iter().enumerate().map(|(at, step)| {
                // Not started yet, a step is still to come, unless its
                // story's task is missing, so it never will.
                let status = task.as_deref().map(|task| {
                    (self.hooks.step_state)(task, at, cx).unwrap_or((
                        if missing {
                            StoryState::Missing
                        } else {
                            StoryState::Queued
                        },
                        None,
                    ))
                });
                (status, step.prompt.as_str())
            }))
            .enumerate()
            .filter(|_| expanded)
            .map(|(at, (status, prompt))| {
                // A line for each step: its number, the first line of its
                // prompt, and its own status.
                gpui_kit::TestSupportExt::test_support(
                    h_flex()
                        .id(SharedString::from(format!(
                            "planning-sent-step-{name}-{at}"
                        )))
                        .w_full()
                        .h(px(24.))
                        .flex_none()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("{}.", at + 1)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(muted)
                                .child(prompt.lines().next().unwrap_or_default().to_string()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .justify_end()
                                .children(self.status_shown(status, current, cx)),
                        ),
                )
            })
            .collect::<Vec<_>>();
        Some(
            v_flex()
                .w_full()
                .gap(ROW_GAP)
                .children(toggle.map(|toggle| own_clicks(div()).child(toggle)))
                .children(rows)
                .into_any_element(),
        )
    }

    fn render_findings(
        &self,
        story: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let now = crate::divergence::now();
        let findings: Vec<Finding> = self
            .notes
            .findings
            .iter()
            .rev()
            .filter(|finding| story.is_none_or(|story| finding.story.as_deref() == Some(story)))
            .cloned()
            .collect();
        // In a story, nothing is listed while it has none; beneath the
        // Backlog, the section says so.
        if findings.is_empty() && story.is_some() {
            return None;
        }
        let rows = findings.into_iter().map(|finding| {
            let file = finding.file.clone();
            let state: AnyElement = match finding.state {
                FindingState::Done => crate::theme::tag(crate::theme::Hue::Green, cx)
                    .text_xs()
                    .child(finding.state.label())
                    .into_any_element(),
                FindingState::Failed => crate::theme::tag(crate::theme::Hue::Red, cx)
                    .text_xs()
                    .child(finding.state.label())
                    .into_any_element(),
                // Ended early: muted, never a coloured state.
                FindingState::Stopped => div()
                    .text_xs()
                    .text_color(muted)
                    .child(finding.state.label())
                    .into_any_element(),
            };
            let focus = self
                .row_focus
                .get(&format!("finding:{file}"))
                .cloned()
                .unwrap_or_else(|| self.focus.clone());
            let focused = focus.is_focused(window);
            let this = cx.entity().downgrade();
            let menu_file = file.clone();
            let menu = Button::new(SharedString::from(format!("planning-findings-menu-{file}")))
                .ghost()
                .xsmall()
                .icon(IconName::Ellipsis)
                .label("More")
                .tooltip("More")
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
                    let (this, file) = (this.clone(), menu_file.clone());
                    menu.item(
                        PopupMenuItem::new("Delete findings").on_click(move |_, _, cx| {
                            this.update(cx, |this, cx| this.delete_findings(&file, cx))
                                .ok();
                        }),
                    )
                });
            let open_file = file.clone();
            let row = h_flex()
                .id(SharedString::from(format!("planning-findings-{file}")))
                .key_context(ROW_CONTEXT)
                .track_focus(&focus)
                .on_action(cx.listener(move |this, _: &ToggleStory, window, cx| {
                    this.open_findings(&open_file, window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &PreviousRow, window, cx| this.step_rows(-1, window, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &NextRow, window, cx| this.step_rows(1, window, cx)),
                )
                .on_action(cx.listener(|this, _: &PreviousColumn, window, cx| {
                    this.step_columns(-1, window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &NextColumn, window, cx| {
                        this.step_columns(1, window, cx)
                    }),
                )
                .relative()
                .child(record_bounds(
                    self.row_bounds.clone(),
                    format!("finding:{file}"),
                ))
                .h(ROW_HEIGHT)
                .px_3()
                .gap_2()
                .items_center()
                .rounded(theme.radius)
                .border_1()
                .border_color(if focused {
                    theme.ring
                } else {
                    gpui_kit::transparent_black()
                })
                .cursor_pointer()
                .hover(|row| row.bg(theme.list_hover))
                .aria_label(format!("Research: {}", finding.topic))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.open_findings(&file, window, cx)),
                )
                .child(Icon::new(IconName::Search).xsmall().text_color(muted))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(format!("Research: {}", finding.topic)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(crate::divergence::ago(finding.ran_at, now)),
                )
                .child(
                    div()
                        .w(STATUS_WIDTH)
                        .flex_none()
                        .flex()
                        .justify_end()
                        .child(state),
                )
                .child(own_clicks(div().flex_none()).child(menu));
            // The steps of the story it was started from, collapsed.
            let steps = finding.story.as_deref().and_then(|of| {
                self.render_story_steps(of, Some(format!("finding:{}", finding.file)), cx)
            });
            v_flex()
                .w_full()
                .gap(ROW_GAP)
                .child(gpui_kit::TestSupportExt::test_support(row))
                .children(steps)
                .into_any_element()
        });
        let mut rows: Vec<AnyElement> = rows.collect();
        // The board's last column, after Done.
        if story.is_none() {
            let heading = h_flex()
                .h(HEADING_HEIGHT)
                .gap_2()
                .items_center()
                .child(column_name(None, cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(self.notes.findings.len().to_string()),
                )
                .into_any_element();
            if self.notes.findings.is_empty() {
                rows.push(
                    gpui_kit::TestSupportExt::test_support(
                        div()
                            .id("planning-empty-findings")
                            .child(empty_line("No findings yet.".into(), muted)),
                    )
                    .into_any_element(),
                );
            }
            let rows = rows.into_iter().map(|row| (row, 1.)).collect();
            return Some(self.render_column(None, "findings", heading, rows, cx));
        }
        Some(
            v_flex()
                .w_full()
                .gap(ROW_GAP)
                .child(
                    div()
                        .pb(HEADING_GAP - ROW_GAP)
                        .child(
                            h_flex()
                                .h(HEADING_HEIGHT)
                                .gap_2()
                                .items_center()
                                .child(div().text_sm().font_semibold().child("Findings"))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(self.notes.findings.len().to_string()),
                                ),
                        )
                        .pb_0(),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    /// The bar across the top of Planning while it is paused for review:
    /// what it waits on, with a link to its findings, and the only two ways
    /// the pause ends.
    fn render_pause_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pause = self.notes.paused.clone()?;
        let theme = cx.theme().clone();
        let warning = theme.warning;
        let file = pause.research.clone();
        let waiting = self.unsent();
        let running = self.running(cx);
        let bar = h_flex()
            .id("planning-paused")
            .key_context("PlanningPauseBar")
            .track_focus(&self.pause_focus)
            .w_full()
            .h(px(40.))
            .flex_none()
            .gap_2()
            .px_3()
            .items_center()
            .rounded(theme.radius)
            .border_1()
            .border_color(warning)
            .bg(warning.opacity(0.12))
            .child(Icon::new(IconName::Pause).small().text_color(warning))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .font_semibold()
                            .text_color(warning)
                            .child("Paused for review"),
                    )
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("waiting on")
                            .child(
                                div()
                                    .id("planning-paused-findings")
                                    .cursor_pointer()
                                    .underline()
                                    .text_color(theme.foreground)
                                    .child(format!("Research: {}", pause.topic))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_findings(&file, window, cx)
                                    })),
                            ),
                    )
                    // What was already running when the pause began runs on.
                    .when(running > 0, |line| {
                        line.child(
                            div()
                                .id("planning-paused-running")
                                .flex_none()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(if running == 1 {
                                    "1 story still running".to_string()
                                } else {
                                    format!("{running} stories still running")
                                }),
                        )
                    }),
            )
            .child(
                Button::new("planning-resume")
                    .ghost()
                    .small()
                    .icon(IconName::Play)
                    .label("Resume")
                    .tooltip("End the pause; nothing is sent")
                    .on_click(cx.listener(|this, _, _, cx| this.resume_planning(cx))),
            )
            .child(
                Button::new("planning-approve")
                    .primary()
                    .small()
                    .icon(IconName::Send)
                    .label("Approve and send sprint")
                    .disabled(waiting == 0 || !self.sections[3].is_empty())
                    .tooltip("End the pause and send Next sprint at once")
                    .on_click(cx.listener(|this, _, window, cx| this.approve_and_send(window, cx))),
            );
        Some(gpui_kit::TestSupportExt::test_support(bar).into_any_element())
    }

    /// The bulk bar, while any card is selected, as the PlanningScope's
    /// parts draw it: how many are selected, and what can be done to them
    /// all, none of it sending anything.
    fn render_bulk_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.selected.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let palette = crate::theme::palette(cx);
        let count = self.selected.len();
        let in_backlog = !self.selected_in(Section::Backlog).is_empty();
        let in_next = !self.selected_in(Section::Next).is_empty();
        let in_draft = !self.selected_in(Section::Draft).is_empty();
        let stories = if count == 1 { "story" } else { "stories" };
        let mode_menu = self
            .bulk_mode_menu
            .map(|highlighted| self.render_mode_menu(None, None, highlighted, cx));
        let actions: AnyElement = if self.confirm_bulk_delete {
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(format!("Delete {count} {stories}?")),
                )
                .child(
                    Button::new("planning-bulk-delete-confirm")
                        .danger()
                        .small()
                        .label("Delete")
                        .tooltip("Delete them for good")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.delete_selected(window, cx)),
                        ),
                )
                .child(
                    Button::new("planning-bulk-delete-keep")
                        .ghost()
                        .small()
                        .label("Cancel")
                        .tooltip("Keep them (Escape)")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_bulk_delete = false;
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else {
            h_flex()
                .gap_1()
                .items_center()
                .child(
                    Button::new("planning-bulk-next")
                        .ghost()
                        .small()
                        .label("Move to next sprint")
                        .disabled(!in_backlog)
                        .tooltip(if in_backlog {
                            "Move the selected stories in the Backlog to the end of Next sprint"
                        } else {
                            "None of the selected stories is in the Backlog"
                        })
                        .on_click(
                            cx.listener(|this, _, _, cx| {
                                this.move_selected(&[Section::Backlog], Section::Next, cx)
                            }),
                        ),
                )
                .child(
                    Button::new("planning-bulk-backlog")
                        .ghost()
                        .small()
                        .label("Move to backlog")
                        .disabled(!in_draft && !in_next)
                        .tooltip(if in_draft || in_next {
                            "Move the selected stories in Draft and Next sprint to the end of the \
                             Backlog"
                        } else {
                            "None of the selected stories is in Draft or Next sprint"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            let from = [Section::Draft, Section::Next];
                            this.move_selected(&from, Section::Backlog, cx)
                        })),
                )
                .child(
                    Button::new("planning-bulk-draft")
                        .ghost()
                        .small()
                        .label("Move to draft")
                        .disabled(!in_backlog)
                        .tooltip(if in_backlog {
                            "Move the selected stories in the Backlog to the end of Draft"
                        } else {
                            "None of the selected stories is in the Backlog"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.move_selected(&[Section::Backlog], Section::Draft, cx)
                        })),
                )
                .child(
                    Button::new("planning-bulk-draft-agent")
                        .ghost()
                        .small()
                        .label("Draft with agent")
                        .tooltip("Draft each selected story with the agent, each in a run of its own, all at once")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let names = this.selected_in_order();
                            this.draft_with_agent(&names, cx)
                        })),
                )
                .child(
                    div().relative().children(mode_menu).child(
                        Button::new("planning-bulk-mode")
                            .ghost()
                            .small()
                            .label("Change mode")
                            .tooltip("Set how every selected story will be sent")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.bulk_mode_menu = match this.bulk_mode_menu {
                                    Some(_) => None,
                                    None => Some(0),
                                };
                                cx.notify();
                            })),
                    ),
                )
                .child(
                    Button::new("planning-bulk-duplicate")
                        .ghost()
                        .small()
                        .label("Duplicate")
                        .tooltip("Duplicate each, just beneath its original")
                        .on_click(cx.listener(|this, _, _, cx| this.duplicate_selected(cx))),
                )
                .child(
                    Button::new("planning-bulk-delete")
                        .ghost()
                        .small()
                        .label("Delete")
                        .tooltip("Delete every selected story, once confirmed")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.bulk_mode_menu = None;
                            this.confirm_bulk_delete = true;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("planning-bulk-clear")
                        .ghost()
                        .small()
                        .label("Clear")
                        .tooltip("Clear the selection (Escape)")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.clear_selection(window, cx)),
                        ),
                )
                .into_any_element()
        };
        let bar = h_flex()
            .id("planning-bulk-bar")
            .key_context("PlanningBulkBar")
            .track_focus(&self.bulk_focus)
            .w_full()
            .h(px(40.))
            .flex_none()
            .gap_2()
            .px_6()
            .items_center()
            .border_t_1()
            .border_color(crate::theme::color(palette.line))
            .bg(crate::theme::color(palette.raised))
            // Its mode picker's options, picked with the arrow keys and
            // Enter while it is open.
            .on_action(cx.listener(|this, _: &PreviousRow, _, cx| this.step_bulk_mode(-1, cx)))
            .on_action(cx.listener(|this, _: &NextRow, _, cx| this.step_bulk_mode(1, cx)))
            .on_action(cx.listener(|this, _: &ToggleStory, _, cx| {
                match this.bulk_mode_menu {
                    Some(at) => this.set_selected_mode(SendMode::ALL[at], cx),
                    None => cx.propagate(),
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(format!("{count} selected")),
            )
            .child(actions);
        Some(gpui_kit::TestSupportExt::test_support(bar).into_any_element())
    }

    /// Up or Down on the bulk bar: its mode picker's highlight moves while
    /// it is open.
    fn step_bulk_mode(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(at) = &mut self.bulk_mode_menu else {
            cx.propagate();
            return;
        };
        let count = SendMode::ALL.len() as isize;
        *at = (*at as isize + step).rem_euclid(count) as usize;
        cx.notify();
    }

    /// A single-story shortcut pressed while no card has focus: it acts on
    /// the card under the pointer, without focusing it, and else goes on.
    fn on_pointed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        act: impl FnOnce(&mut Self, &str, &mut Window, &mut Context<Self>),
    ) {
        match self.pointed_card(window) {
            Some(name) => act(self, &name, window, cx),
            None => cx.propagate(),
        }
    }

    /// The drafting panel: the request, the run while it goes, its error,
    /// the proposals, the notes of those decided, and the Undos of those
    /// rejected.
    /// The side panel, as the PlanningScope's layout places it: the open
    /// story, or else drafting or research, while either is wanted; one
    /// thing at a time.
    fn render_side_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.loaded {
            return None;
        }
        if let Some(name) = self.open.as_ref().map(|open| open.name.clone()) {
            let steps = self.story(&name).map_or(0, |story| story.steps().len());
            for at in 0..steps {
                self.step_focus(&name, at, cx);
            }
            let cited = self.story(&name).map_or(0, |story| story.spec.len());
            for at in 0..cited {
                self.row_focus(&format!("cite:{name}:{at}"), cx);
            }
            let findings = self.render_findings(Some(&name), window, cx);
            let proposals = self.render_story_proposals(&name, window, cx);
            return Some(self.render_open(&name, proposals, findings, window, cx));
        }
        if self.drafting.is_none() && self.proposals.is_empty() {
            return None;
        }
        // Proposals kept from before come back in their panel.
        if self.drafting.is_none() {
            let about = self.panel_about();
            self.open_draft_panel(about, false, window, cx);
        }
        Some(self.render_draft_panel(window, cx))
    }

    /// A panel, as the PlanningScope's parts say: the side panel, docked at
    /// the board's right, 400 pixels wide and as tall as the tab, on the
    /// raised surface, its corners square, a 1 pixel line along its left,
    /// with 12 pixels of padding; its heading stays at its top and its
    /// footer at its foot while its content scrolls between them.
    fn panel_box(
        &self,
        id: impl Into<SharedString>,
        heading: Div,
        content: Div,
        footer: Div,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id: SharedString = id.into();
        let handle = self.panel_scroll.clone();
        let overflows = handle.max_offset().y > px(0.);
        let body = div()
            .id(SharedString::from(format!("{id}-content")))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&handle)
            .child(content.w_full().px_3().pb_2());
        let body = if overflows {
            crate::scrollbar::with_scrollbar(id.clone(), &handle, body, true, None, cx)
        } else {
            body.into_any_element()
        };
        // Once its content comes to hold more than fits, or fits again, the
        // scrollbar is shown, or hidden, from the next frame.
        let this = cx.entity().downgrade();
        let watch = canvas(
            move |_, _, cx| {
                if (handle.max_offset().y > px(0.)) != overflows {
                    let this = this.clone();
                    cx.defer(move |cx| {
                        this.update(cx, |_, cx| cx.notify()).ok();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_0();
        v_flex()
            .id(id)
            .relative()
            .w(PANEL_WIDTH)
            .flex_none()
            .h_full()
            .min_h_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(crate::theme::color(crate::theme::palette(cx).raised))
            .child(div().flex_none().px_3().pt_3().pb_2().child(heading))
            .child(div().relative().flex_1().min_h_0().w_full().child(body))
            .child(div().flex_none().px_3().pt_2().pb_3().child(footer))
            .child(watch)
    }

    /// A story's own draft that failed, on its card or in the open story:
    /// what went wrong in plain words, its details collapsed, and Try
    /// again, which drafts the story again.
    fn render_draft_error(
        &self,
        name: &str,
        error: String,
        details_open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let (details_name, retry_name) = (name.to_string(), name.to_string());
        gpui_kit::TestSupportExt::test_support(
            v_flex()
                .id(SharedString::from(format!("planning-story-draft-error-{name}")))
                .w_full()
                .mt_1()
                .gap_1()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(theme.danger)
                                .child("The agent couldn't draft this story."),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "planning-story-draft-details-{name}"
                            )))
                            .ghost()
                            .xsmall()
                            .icon(if details_open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .label("Details")
                            .tooltip(if details_open {
                                "Hide what was reported"
                            } else {
                                "Show what was reported"
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(draft) = this.story_drafts.get_mut(&details_name) {
                                    draft.details_open = !draft.details_open;
                                }
                                cx.notify();
                            })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "planning-story-draft-retry-{name}"
                            )))
                            .ghost()
                            .xsmall()
                            .label("Try again")
                            .tooltip("Draft this story with the agent again")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.draft_story(&retry_name, cx)
                            })),
                        ),
                )
                .when(details_open, |details| {
                    details.child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .font_family(theme.mono_font_family.clone())
                            .child(error.trim().to_string()),
                    )
                }),
        )
        .into_any_element()
    }

    /// The open story's own draft with the agent, beneath its fields: its
    /// run while it goes, and its proposals waiting, under "Proposals", as
    /// proposal cards, with Accept all and Reject all beneath the last, the
    /// notes of those decided, and the Undos of those rejected. None while
    /// there is nothing of it to show.
    fn render_story_proposals(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let drafting = self.story_drafting(name);
        let error = self
            .story_drafts
            .get(name)
            .and_then(|draft| draft.error.clone().map(|error| (error, draft.details_open)));
        let waiting = self.waiting(name).to_vec();
        let decided: Vec<(String, String)> = self
            .decided
            .iter()
            .filter(|decided| decided.owner.as_deref() == Some(name))
            .map(|decided| (decided.text.clone(), decided.story.clone()))
            .collect();
        let undos: Vec<(String, String)> = self
            .rejected
            .iter()
            .filter(|rejected| rejected.owner.as_deref() == Some(name))
            .map(|rejected| (rejected.proposal.id.clone(), rejected.proposal.story_title()))
            .collect();
        if !drafting && error.is_none() && waiting.is_empty() && decided.is_empty() && undos.is_empty()
        {
            return None;
        }
        let mut list = v_flex()
            .id(SharedString::from(format!("planning-story-proposals-{name}")))
            .gap_2()
            .child(div().text_xs().text_color(muted).child("Proposals"));
        if drafting {
            let stop = name.to_string();
            list = list.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(loading_line("Drafting…".into(), muted).flex_1())
                    .child(
                        Button::new(SharedString::from(format!("planning-story-draft-stop-{name}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Square)
                            .label("Stop")
                            .tooltip("Stop this story's run, keeping what it has proposed")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.stop_story_draft(&stop, cx)
                            })),
                    ),
            );
        }
        if let Some((error, details_open)) = error {
            list = list.child(self.render_draft_error(name, error, details_open, cx));
        }
        for (at, (text, story)) in decided.into_iter().enumerate() {
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("planning-story-decided-{name}-{at}")))
                    .gap_1()
                    .text_xs()
                    .text_color(muted)
                    .child(Icon::new(IconName::Check).xsmall())
                    .child(
                        div()
                            .id(SharedString::from(format!("planning-story-decided-link-{name}-{at}")))
                            .cursor_pointer()
                            .underline()
                            .child(text)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_story(&story, window, cx)
                            })),
                    ),
            );
        }
        for proposal in &waiting {
            list = list.child(self.render_proposal(proposal, window, cx));
        }
        for (id, title) in undos {
            list = list.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Rejected “{title}”."))
                    .child(
                        Button::new(SharedString::from(format!("planning-undo-{id}")))
                            .ghost()
                            .xsmall()
                            .label("Undo")
                            .tooltip("Bring it back")
                            .on_click(cx.listener(move |this, _, _, cx| this.undo_reject(&id, cx))),
                    ),
            );
        }
        if !waiting.is_empty() {
            let (accept, reject) = (name.to_string(), name.to_string());
            list = list.child(
                h_flex()
                    .gap_1()
                    .justify_end()
                    .child(
                        Button::new(SharedString::from(format!("planning-story-reject-all-{name}")))
                            .ghost()
                            .small()
                            .label("Reject all")
                            .tooltip("Let every proposal for this story go")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.reject_all(Some(&reject), cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("planning-story-accept-all-{name}")))
                            .ghost()
                            .small()
                            .label("Accept all")
                            .tooltip("Record every proposal citing the spec in the plan, in order")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.accept_all(Some(&accept), cx)
                            })),
                    ),
            );
        }
        Some(gpui_kit::TestSupportExt::test_support(list).into_any_element())
    }

    fn render_draft_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let Some(drafting) = &self.drafting else {
            return div().into_any_element();
        };
        let running = drafting.running;
        let activity = activity_of(&drafting.activity).clone();
        let error = drafting.error.clone();
        let details_open = drafting.details_open;
        let proposed_nothing = drafting.proposed_nothing;
        let input = drafting.input.clone();
        // Nothing to send: only spaces and new lines, or nothing at all.
        let empty = input.read(cx).value().trim().is_empty();
        let about = drafting.about.is_some();
        let research = drafting.research;
        let count = self.proposals.len();
        // It closes once nothing in it is left to decide or run.
        let closable = running.is_none() && count == 0 && !self.panel_rejected();
        let name = if research {
            "Research"
        } else {
            "Draft stories"
        };
        let heading = panel_heading(
            name.into(),
            closable.then(|| {
                Button::new("planning-draft-close")
                    .ghost()
                    .xsmall()
                    .label("Close")
                    .tooltip("Close this panel (Escape)")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.drafting = None;
                        this.focus.focus(window, cx);
                        cx.notify();
                    }))
                    .into_any_element()
            }),
        );
        let mut content = v_flex()
            .gap_2()
            .child(div().text_xs().text_color(muted).child(if research {
                "What should be looked into?"
            } else if about {
                "What does this story need?"
            } else {
                "Describe what you want"
            }))
            .child(Textarea::new(&input).disabled(running.is_some()));
        // Loading: what the run is doing, where its result will appear.
        if let Some(started) = running {
            let elapsed = crate::subagents::format_elapsed(started.elapsed());
            content = content.child(gpui_kit::TestSupportExt::test_support(
                h_flex()
                    .id("planning-draft-running")
                    .gap_2()
                    .items_center()
                    .child(loading_line(format!("{activity} {elapsed}"), muted).flex_1())
                    .child(
                        Button::new("planning-draft-stop")
                            .ghost()
                            .small()
                            .icon(IconName::Square)
                            .label("Stop")
                            .tooltip(if research {
                                "Stop the research; its findings say it ended early"
                            } else {
                                "Stop the agent, proposing nothing"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.stop_draft(cx))),
                    ),
            ));
        }
        // Error: what went wrong, in plain words, its details collapsed, and
        // Try again, the request kept in its box.
        if let Some(error) = error {
            content = content.child(gpui_kit::TestSupportExt::test_support(
                v_flex()
                    .id("planning-draft-error")
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().text_sm().text_color(theme.danger).child(
                                if research {
                                    "The research couldn't be done."
                                } else {
                                    "The agent couldn't propose stories from that."
                                },
                            ))
                            .child(
                                Button::new("planning-draft-details")
                                    .ghost()
                                    .xsmall()
                                    .icon(if details_open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .label("Details")
                                    .tooltip(if details_open {
                                        "Hide what was reported"
                                    } else {
                                        "Show what was reported"
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(drafting) = &mut this.drafting {
                                            drafting.details_open = !drafting.details_open;
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("planning-draft-retry")
                                    .ghost()
                                    .xsmall()
                                    .label("Try again")
                                    .tooltip("Run it again with the same request")
                                    .on_click(cx.listener(|this, _, _, cx| this.draft(cx))),
                            ),
                    )
                    .when(details_open, |details| {
                        details.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .font_family(theme.mono_font_family.clone())
                                .child(error.trim().to_string()),
                        )
                    }),
            ));
        }
        // Empty: nothing proposed, with Draft again.
        if proposed_nothing && count == 0 && running.is_none() {
            content = content.child(gpui_kit::TestSupportExt::test_support(
                h_flex()
                    .id("planning-proposed-nothing")
                    .w_full()
                    .h(ROW_HEIGHT)
                    .gap_2()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(muted)
                    .child("No stories proposed.")
                    .child(
                        Button::new("planning-draft-again")
                            .ghost()
                            .xsmall()
                            .label("Draft again")
                            .tooltip("Run the draft again with the same request")
                            .on_click(cx.listener(|this, _, _, cx| this.draft(cx))),
                    ),
            ));
        }
        // The notes of those decided.
        for (at, decided) in self.decided.iter().enumerate() {
            if decided.owner.is_some() {
                continue;
            }
            let story = decided.story.clone();
            content = content.child(
                h_flex()
                    .id(("planning-decided", at))
                    .gap_1()
                    .text_xs()
                    .text_color(muted)
                    .child(Icon::new(IconName::Check).xsmall())
                    .child(
                        div()
                            .id(("planning-decided-link", at))
                            .cursor_pointer()
                            .underline()
                            .child(decided.text.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_story(&story, window, cx)
                            })),
                    ),
            );
        }
        // Each proposal, as a card, in the order given, 8 pixels apart.
        let proposals = self.proposals.clone();
        let mut cards = v_flex().gap_2();
        for proposal in &proposals {
            cards = cards.child(self.render_proposal(proposal, window, cx));
        }
        content = content.child(cards);
        // The Undos of those rejected.
        let undos: Vec<(String, String)> = self
            .rejected
            .iter()
            .filter(|rejected| rejected.owner.is_none())
            .map(|rejected| {
                (
                    rejected.proposal.id.clone(),
                    rejected.proposal.story_title(),
                )
            })
            .collect();
        for (id, title) in undos {
            content = content.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Rejected “{title}”."))
                    .child({
                        let id = id.clone();
                        Button::new(SharedString::from(format!("planning-undo-{id}")))
                            .ghost()
                            .xsmall()
                            .label("Undo")
                            .tooltip("Bring it back")
                            .on_click(cx.listener(move |this, _, _, cx| this.undo_reject(&id, cx)))
                    }),
            );
        }
        // The footer: right-aligned, the primary action last.
        let footer = h_flex()
            .gap_1()
            .justify_end()
            .when(count > 0, |footer| {
                footer
                    .child(
                        Button::new("planning-reject-all")
                            .ghost()
                            .small()
                            .label("Reject all")
                            .tooltip("Let every proposal go")
                            .on_click(cx.listener(|this, _, _, cx| this.reject_all(None, cx))),
                    )
                    .child(
                        Button::new("planning-accept-all")
                            .ghost()
                            .small()
                            .label(match count {
                                1 => "Accept 1".to_string(),
                                n => format!("Accept all {n}"),
                            })
                            .tooltip("Record every proposal citing the spec in the plan, in order")
                            .on_click(cx.listener(|this, _, _, cx| this.accept_all(None, cx))),
                    )
            })
            .child(
                Button::new("planning-draft-run")
                    .primary()
                    .small()
                    .icon(if research {
                        IconName::Search
                    } else {
                        IconName::Sparkles
                    })
                    .label(if research {
                        "Research"
                    } else if count == 0 {
                        "Draft"
                    } else {
                        "Draft more"
                    })
                    .disabled(running.is_some() || empty)
                    .tooltip(if research {
                        format!("Have the agent look into it, reading only ({MOD}Enter)")
                    } else {
                        "Have the agent propose stories (Enter)".to_string()
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.draft(cx))),
            );
        let panel = self
            .panel_box("planning-draft-panel", heading, content, footer, cx)
            .track_focus(&self.panel_focus);
        gpui_kit::TestSupportExt::test_support(panel).into_any_element()
    }
}

/// A panel's heading row, 32 pixels tall: its name, and its close button
/// where it has one.
fn panel_heading(name: SharedString, close: Option<AnyElement>) -> Div {
    h_flex()
        .h(HEADING_HEIGHT)
        .gap_2()
        .items_center()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .font_semibold()
                .child(name),
        )
        .children(close)
}

/// A card, as the PlanningScope's parts say: a box on the base surface, a 1
/// pixel line, rounded 4 pixels, with 12 pixels of padding.
fn card_box(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    v_flex()
        .id(id)
        .w_full()
        .gap_1()
        .p_3()
        .rounded(px(4.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
}

/// A skeleton card, while the plan is read: a card's box, with muted bars
/// where its grip, mode pill, and title will be.
fn skeleton_card(cx: &App) -> Div {
    let theme = cx.theme();
    let muted = theme.muted;
    v_flex()
        .w_full()
        .gap_2()
        .p_3()
        .rounded(px(6.))
        .border_1()
        .border_color(theme.border)
        .bg(crate::theme::color(crate::theme::palette(cx).raised))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().w(GRIP_WIDTH).h(px(10.)).rounded(px(3.)).bg(muted))
                .child(div().w(PILL_WIDTH).h(px(14.)).rounded(px(4.)).bg(muted)),
        )
        .child(div().w_full().h(px(12.)).rounded(px(3.)).bg(muted))
}

/// A piece of research's findings, read whole in a body tab of its own, as
/// the PlanningScope's research says: its markdown rendered, its links
/// opening what they name.
pub struct FindingsView {
    path: PathBuf,
    title: SharedString,
    text: SharedString,
    open: crate::file_link::OpenFile,
    scroll: ScrollHandle,
}

impl FindingsView {
    pub fn new(path: PathBuf, title: String, open: crate::file_link::OpenFile) -> Self {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| format!("# {title}\n\nThese findings can no longer be read."));
        Self {
            path,
            title: title.into(),
            text: text.into(),
            open,
            scroll: ScrollHandle::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }
}

impl Render for FindingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.open.clone();
        let view = gpui_kit::component::text::TextView::markdown("findings", self.text.clone())
            .on_link_click(move |url, _, window, cx| {
                crate::file_link::open_link(url, &open, window, cx)
            });
        gpui_kit::TestSupportExt::test_support(
            div()
                .id("planning-findings-view")
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .bg(cx.theme().background)
                .child(
                    h_flex()
                        .w_full()
                        .justify_center()
                        .child(div().w_full().max_w(READING_WIDTH).p_6().child(view)),
                ),
        )
    }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(
        || path.to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl Render for Planning {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The open story rewritten: its editors take what it now says.
        if std::mem::take(&mut self.refresh_open)
            && let Some(open) = &self.open
            && let Some(story) = self.story(&open.name)
        {
            let (title, prompt) = (story.title.clone(), story.prompt.clone());
            let (title_input, prompt_input) = (open.title.clone(), open.prompt.clone());
            title_input.update(cx, |input, cx| input.set_value(title, window, cx));
            prompt_input.update(cx, |editor, cx| editor.set_value(prompt, window, cx));
        }
        if !cx.has_active_drag() {
            // Let go anywhere but over the board, or cancelled, the card
            // settles back where it came from.
            if let Some(lift) = self.lift.take() {
                let pointer = self.dragged.borrow().as_ref().map(|(_, at)| *at);
                if let Some(pointer) = pointer {
                    self.settle(lift, pointer, None);
                }
            }
            self.slots.clear();
            self.gap.set(None);
            *self.dragged.borrow_mut() = None;
            self.drag_carried.borrow_mut().clear();
        } else {
            self.sync_slots();
        }
        self.slides.frame(self.slide_next.take());
        if self
            .settle
            .as_ref()
            .is_some_and(|settle| crate::animations::slide::ended(settle.since))
        {
            self.settle = None;
        }
        if self.settle.is_some() || self.slots.iter().any(|slot| slot.open.moving()) {
            window.request_animation_frame();
        }
        if let Some(open) = &mut self.open {
            open.fit.keep_cursor_in_view(&open.prompt, window, cx);
        }
        if self.any_running(cx) {
            self.keep_ticking(cx);
        }
        let files: Vec<String> = self
            .notes
            .findings
            .iter()
            .map(|finding| finding.file.clone())
            .collect();
        for file in files {
            self.row_focus(&format!("finding:{file}"), cx);
        }
        let save_error = self.save_error.lock().unwrap().clone().map(|why| {
            gpui_kit::TestSupportExt::test_support(
                h_flex()
                    .id("planning-save-error")
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(why),
                    )
                    .child(
                        Button::new("planning-save-retry")
                            .ghost()
                            .xsmall()
                            .label("Try again")
                            .tooltip("Save the plan again")
                            .on_click(cx.listener(|this, _, _, cx| {
                                *this.save_error.lock().unwrap() = None;
                                this.save_order(cx);
                                let names: Vec<String> = this
                                    .sections
                                    .iter()
                                    .flatten()
                                    .map(|story| story.name.clone())
                                    .collect();
                                for name in names {
                                    this.save_story_named(&name, cx);
                                }
                                cx.notify();
                            })),
                    ),
            )
        });
        self.keep_selection();
        // Each column records where it is drawn anew, so a column no longer
        // drawn is not found under the pointer.
        self.column_bounds.borrow_mut().clear();
        let pause = self.render_pause_bar(cx);
        let side = self.render_side_panel(window, cx);
        // Along the board's foot while any card is selected, sliding up as
        // the columns shorten, and away again.
        let bulk = self.render_bulk_bar(cx);
        self.bulk_shown.set(bulk.is_some());
        if self.bulk_shown.moving() {
            window.request_animation_frame();
        }
        let bulk_shown = self.bulk_shown.now();
        let bulk = (bulk_shown > 0.).then(|| {
            div()
                .flex_none()
                .w_full()
                .h(px(40.) * bulk_shown)
                .overflow_hidden()
                .children(bulk)
        });
        // The side panel slides in from the right as it opens, and back out
        // as it closes, the board narrowing and widening alongside it;
        // showing something else in it doesn't slide.
        // Coming back to its tab, it is already where it went meanwhile.
        let was_shown = self.panel_shown.now();
        if self.returning {
            self.panel_shown = crate::animations::slide::Toward::at(side.is_some());
        } else {
            self.panel_shown.set(side.is_some());
        }
        let narrowing = if self.panel_shown.is_open() { 1. - was_shown } else { 0. };
        self.reveal_open(PANEL_WIDTH * narrowing, window);
        self.returning = false;
        self.glide(window);
        let shown = self.panel_shown.now();
        if self.panel_shown.moving() {
            window.request_animation_frame();
        }
        let side = (shown > 0.).then(|| {
            let side = side.unwrap_or_else(|| {
                div()
                    .size_full()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .bg(crate::theme::color(crate::theme::palette(cx).raised))
                    .into_any_element()
            });
            div()
                .flex_none()
                .h_full()
                .w(PANEL_WIDTH * shown)
                .overflow_hidden()
                .child(div().w(PANEL_WIDTH).h_full().child(side))
        });
        // A dropped card's copy, settling into its place: it straightens,
        // its shadow fades, and it moves from where it was let go to where
        // the card is drawn.
        let settling = self.settle.as_ref().map(|settle| {
            let done = crate::animations::slide::eased(settle.since);
            let to = self
                .row_bounds
                .borrow()
                .get(&settle.name)
                .map_or(settle.from, |bounds| bounds.origin);
            let at = settle.from + (to - settle.from) * done;
            let app: &App = cx;
            deferred(anchored().position(at).child(crate::animations::tilt::tilted(
                settle.size,
                TILT * (1. - done),
                || card_copy(&settle.face, 1. - done, app).into_any_element(),
            )))
        });
        // Every column, always, in the order work moves, then Findings.
        let mut columns: Vec<AnyElement> = SECTIONS
            .into_iter()
            .map(|section| self.render_section(section, window, cx))
            .collect();
        columns.extend(self.render_findings(None, window, cx));
        // Across the top of the whole board, above every column.
        let bars = (save_error.is_some() || pause.is_some()).then(|| {
            v_flex()
                .w_full()
                .flex_none()
                .gap_2()
                .px_6()
                .pt_6()
                .children(save_error)
                .children(pause)
        });
        // The columns side by side, 24 pixels in from the board's top,
        // sides, and foot, scrolling sideways when wider than the tab; a
        // card dragged over it finds its gap in the column under the
        // pointer, and held near its left or right edge, or near a
        // column's top or bottom, scrolls it.
        let board = h_flex()
            .id("planning-board")
            .size_full()
            .overflow_x_scroll()
            .track_scroll(&self.board_scroll)
            .p_6()
            .gap(COLUMN_GAP)
            .items_stretch()
            // Clicked while the focus is elsewhere, the board takes it, so
            // its shortcuts reach the card under the pointer.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.focus.contains_focused(window, cx) {
                        this.focus.focus(window, cx);
                    }
                }),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<StoryDrag>, _, cx| {
                    this.drag_moved(event, cx)
                }),
            )
            .on_drop(
                cx.listener(|this, drag: &StoryDrag, window, cx| this.drop_story(drag, window, cx)),
            )
            .children(columns);
        let board = crate::scrollbar::with_horizontal_scrollbar(
            "planning-board",
            &self.board_scroll,
            board,
            cx,
        );
        // Each frame, once the cards are where they are drawn now, however
        // the board or a column has scrolled: where the board is, and the
        // gap a dragged card would land in, chosen anew.
        let board_frame = {
            let (board_bounds, dragged, gap, carried) = (
                self.board_bounds.clone(),
                self.dragged.clone(),
                self.gap.clone(),
                self.drag_carried.clone(),
            );
            let (wheel_from, board_scroll, column_scroll) = (
                self.wheel_from.clone(),
                self.board_scroll.clone(),
                self.column_scroll.clone(),
            );
            let (columns, column_bounds, row_bounds) = (
                self.drop_columns(),
                self.column_bounds.clone(),
                self.row_bounds.clone(),
            );
            let this = cx.entity().downgrade();
            canvas(
                move |bounds, window, cx| {
                    board_bounds.set(Some(bounds));
                    let chosen = dragged.borrow().as_ref().and_then(|(name, at)| {
                        choose_gap(
                            name,
                            &carried.borrow(),
                            *at,
                            &columns,
                            &column_bounds.borrow(),
                            &row_bounds.borrow(),
                        )
                    });
                    if chosen != gap.get() {
                        gap.set(chosen);
                        let this = this.clone();
                        cx.defer(move |cx| {
                            this.update(cx, |_, cx| cx.notify()).ok();
                        });
                    }
                    window.insert_hitbox(bounds, HitboxBehavior::Normal)
                },
                // A wheel turn or swipe over the board, before anything
                // takes it: where the board and each column were scrolled
                // to, for [`Self::wheel`] to start from.
                move |_, hitbox, window, _| {
                    window.on_mouse_event(move |_: &ScrollWheelEvent, phase, window, _| {
                        if phase != DispatchPhase::Capture {
                            return;
                        }
                        *wheel_from.borrow_mut() = hitbox.should_handle_scroll(window).then(|| {
                            let columns = column_scroll
                                .iter()
                                .map(|(key, handle)| (*key, handle.offset()))
                                .collect();
                            (board_scroll.offset(), columns)
                        });
                    });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };
        let planning = v_flex()
            .id("planning")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .on_action(
                cx.listener(|this, _: &NewStory, window, cx| this.new_story_here(window, cx)),
            )
            .on_action(cx.listener(|this, _: &SendSprint, window, cx| this.send_sprint(window, cx)))
            .on_action(cx.listener(|this, _: &OpenDraftStories, window, cx| {
                this.draft_stories(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &OpenResearch, window, cx| this.research(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NextRegion, window, cx| this.next_region(window, cx)))
            // With no card focused, the card under the pointer.
            .on_action(cx.listener(|this, _: &ToggleStory, window, cx| {
                // A mode picker or a confirmation opened from the card under
                // the pointer answers Enter first.
                if this.mode_menu.is_some() || this.confirm_delete.is_some() {
                    return this.enter_row(window, cx);
                }
                this.on_pointed(window, cx, |this, name, window, cx| {
                    this.toggle_story(name, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &PreviousRow, window, cx| {
                if this.mode_menu.is_some() {
                    this.step_rows(-1, window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &NextRow, window, cx| {
                if this.mode_menu.is_some() {
                    this.step_rows(1, window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSelected, window, cx| {
                this.on_pointed(window, cx, |this, name, _, cx| this.toggle_selected(name, cx))
            }))
            .on_action(cx.listener(|this, _: &PickMode, window, cx| {
                this.on_pointed(window, cx, |this, name, _, cx| this.open_mode_menu(name, cx))
            }))
            .on_action(cx.listener(|this, _: &ToggleSprint, window, cx| {
                this.on_pointed(window, cx, |this, name, _, cx| match this.find(name) {
                    Some((Section::Next, _)) => this.move_story(name, Section::Backlog, 0, cx),
                    Some((Section::Draft, _)) => this.move_to(name, Section::Backlog, cx),
                    Some((Section::Backlog | Section::Done, _)) => {
                        this.move_to(name, Section::Next, cx)
                    }
                    _ => {}
                })
            }))
            .on_action(cx.listener(|this, _: &DeleteStory, window, cx| {
                this.on_pointed(window, cx, |this, name, _, cx| this.ask_delete(name, cx))
            }))
            .on_action(cx.listener(|this, _: &MoveStoryUp, window, cx| {
                this.on_pointed(window, cx, |this, name, _, cx| this.shift_story(name, true, cx))
            }))
            .on_action(cx.listener(|this, _: &MoveStoryDown, window, cx| {
                this.on_pointed(window, cx, |this, name, _, cx| this.shift_story(name, false, cx))
            }))
            .on_action(cx.listener(|this, _: &MoveStoryLeft, window, cx| {
                this.on_pointed(window, cx, |this, name, _, cx| {
                    this.shift_story_across(name, false, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &MoveStoryRight, window, cx| {
                this.on_pointed(window, cx, |this, name, _, cx| {
                    this.shift_story_across(name, true, cx)
                })
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                // Escape while dragging cancels it, the card settling back.
                if this.lift.is_some() && cx.stop_active_drag(window) {
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if this.escape(window, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_action(
                cx.listener(|this, _: &crate::main_window::Dismiss, window, cx| {
                    if !this.escape(window, cx) {
                        cx.propagate();
                    }
                }),
            )
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                // Enter accepts an open completion rather than adding a line;
                // in the drafting panel, Enter drafts, as Ctrl/Cmd+Enter does,
                // and Shift+Enter adds a line; research starts only on
                // Ctrl/Cmd+Enter. In the box, Enter never reaches the list.
                if let Some(open) = &this.open
                    && open.completion.read(cx).is_open()
                    && !action.secondary
                {
                    cx.stop_propagation();
                    open.completion
                        .update(cx, |menu, cx| menu.accept_selected(window, cx));
                } else if !action.secondary
                    && this.open_step.as_ref().is_some_and(|step| {
                        step.prompt.read(cx).focus_handle(cx).is_focused(window)
                    })
                {
                    cx.stop_propagation();
                    this.step_entered(window, cx);
                } else if let Some(drafting) = this
                    .drafting
                    .as_ref()
                    .filter(|drafting| drafting.input.read(cx).focus_handle(cx).is_focused(window))
                {
                    let sends = action.secondary || (!drafting.research && !action.shift);
                    if sends {
                        cx.stop_propagation();
                        this.draft(cx);
                    }
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                if let Some(open) = &this.open
                    && open.completion.read(cx).is_open()
                {
                    cx.stop_propagation();
                    open.completion
                        .update(cx, |menu, cx| menu.select_next(-1, window, cx));
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                if let Some(open) = &this.open
                    && open.completion.read(cx).is_open()
                {
                    cx.stop_propagation();
                    open.completion
                        .update(cx, |menu, cx| menu.select_next(1, window, cx));
                }
            }))
            // The board, narrowed to the space left of the side panel while
            // it is open, the pause bar above the board, beside the panel.
            .child(
                h_flex()
                    .size_full()
                    .min_h_0()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .children(bars)
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_h_0()
                                    .w_full()
                                    // Once the board and its columns have
                                    // had the wheel, as the layout says
                                    // instead.
                                    .on_scroll_wheel(cx.listener(|this, event, window, cx| {
                                        this.wheel(event, window, cx)
                                    }))
                                    .child(board)
                                    .child(board_frame)
                                    .children(settling),
                            )
                            .children(bulk),
                    )
                    .children(side),
            );
        gpui_kit::TestSupportExt::test_support(planning)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, Focusable as _, SharedString, TestAppContext,
    };

    use super::{
        CitationKind, Planning, PlanningHooks, Proposal, Section, SendMode, Story, StoryState,
        draft_request, load, load_checked, order_of, parse_proposals, parse_story_block,
        save_order_text, save_story, section_ix, story_example, story_file,
    };
    use crate::divergence::Cancel;

    fn project(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(name);
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    type Run = fn(&Path, &str, &Cancel, &dyn Fn(String)) -> anyhow::Result<String>;

    fn hooks(draft: Run) -> PlanningHooks {
        PlanningHooks {
            state: Rc::new(|_, _| None),
            step_state: Rc::new(|_, _, _| None),
            send: Rc::new(|_, _| {}),
            open_task: Rc::new(|_, _, _| {}),
            resume: Rc::new(|_, _, _| {}),
            draft,
            research: draft,
            open_findings: Rc::new(|_, _, _, _| {}),
            open_source: Rc::new(|_, _, _, _| {}),
            completions: Rc::new(crate::chat_input::prompt_completions),
        }
    }

    fn unused(_: &Path, _: &str, _: &Cancel, _: &dyn Fn(String)) -> anyhow::Result<String> {
        anyhow::bail!("unused")
    }

    /// Each column's top edge, in both modes: its status icon's colour,
    /// the theme's role for it, laid over the recessed surface at half
    /// strength, 2 pixels tall, inside the list's 8 pixel corners.
    #[test]
    fn each_column_has_its_icon_colour_along_its_top() {
        use super::{COLUMN_EDGE, COLUMN_GAP, COLUMN_WIDTH, LIST_RADIUS, column_color, column_edge};
        use crate::theme::{DARK, LIGHT, color};
        use gpui_kit::px;
        assert_eq!(COLUMN_WIDTH, px(320.));
        assert_eq!(COLUMN_GAP, px(16.));
        assert_eq!(LIST_RADIUS, px(8.));
        assert_eq!(COLUMN_EDGE, px(2.));
        for palette in [&DARK, &LIGHT] {
            let columns = [
                (Some(Section::Draft), palette.text_tertiary),
                (Some(Section::Backlog), palette.text_tertiary),
                (Some(Section::Next), palette.text_secondary),
                (Some(Section::Current), palette.accent),
                (Some(Section::Done), palette.success),
                (None, palette.text_secondary),
            ];
            for (section, role) in columns {
                assert_eq!(column_color(section, palette), color(role), "{section:?}");
                assert_eq!(column_edge(section, palette), color(role).opacity(0.5), "{section:?}");
            }
            // Neighbours never share a look, but Draft and the Backlog, told
            // apart by their icons.
            for pair in columns.windows(2).skip(1) {
                assert_ne!(
                    column_edge(pair[0].0, palette),
                    column_edge(pair[1].0, palette),
                    "{:?} and {:?}",
                    pair[0].0,
                    pair[1].0,
                );
            }
        }
    }

    /// A column's edge stays below state: half strength and 2 pixels, where
    /// a card's hover, selected surface, focus ring, and the open story's
    /// accent line are all at full strength, the line 3 pixels wide.
    #[test]
    fn a_columns_edge_is_quieter_than_any_cards_state() {
        use super::{COLUMN_EDGE, column_edge};
        use crate::theme::{DARK, LIGHT, color};
        use gpui_kit::px;
        for palette in [&DARK, &LIGHT] {
            let states = [palette.hover, palette.selected, palette.accent];
            for section in [
                Some(Section::Draft),
                Some(Section::Backlog),
                Some(Section::Next),
                Some(Section::Current),
                Some(Section::Done),
                None,
            ] {
                let edge = column_edge(section, palette);
                for state in states {
                    assert!(edge.a < color(state).a, "{section:?}");
                }
            }
        }
        assert!(COLUMN_EDGE < px(3.));
    }

    /// A list is pressed into the board's base: its top and left edges
    /// barely darker, its bottom and right a touch lighter, in both modes.
    #[test]
    fn a_list_is_pressed_into_the_base() {
        use super::LIST_BEVEL;
        use crate::theme::{Bevel, DARK, LIGHT, bevel_edges};
        use gpui_kit::{Bounds, point, px, size};
        assert_eq!(LIST_BEVEL, Bevel::Pressed);
        let list = Bounds::new(point(px(0.), px(0.)), size(px(320.), px(600.)));
        for palette in [&DARK, &LIGHT] {
            let raised = bevel_edges(list, Bevel::Raised, palette);
            let pressed = bevel_edges(list, Bevel::Pressed, palette);
            // Top and left take the raised bevel's shade, bottom and right
            // its light.
            assert_eq!(pressed[0].1, raised[2].1);
            assert_eq!(pressed[1].1, raised[2].1);
            assert_eq!(pressed[2].1, raised[0].1);
            assert_eq!(pressed[3].1, raised[0].1);
        }
    }

    /// Planning, alone in a window, for the project at `dir`.
    fn open_planning(
        dir: PathBuf,
        draft: Run,
        cx: &mut TestAppContext,
    ) -> (Entity<Planning>, AnyWindowHandle) {
        open_planning_with(dir, hooks(draft), cx)
    }

    /// Planning, alone in a window, for the project at `dir`, with `hooks`.
    fn open_planning_with(
        dir: PathBuf,
        hooks: PlanningHooks,
        cx: &mut TestAppContext,
    ) -> (Entity<Planning>, AnyWindowHandle) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::piton_syntax::init();
            super::bind_keys(cx);
        });
        let mut planning = None;
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|cx| Planning::new(dir, hooks, cx));
            planning = Some(view.clone());
            Root::new(view, window, cx)
        });
        // The plan is read in the background.
        cx.run_until_parked();
        (planning.unwrap(), window.into())
    }

    /// A project whose Backlog holds stories titled `titles`, in order.
    fn backlog(name: &str, titles: &[&str]) -> (PathBuf, Vec<String>) {
        let dir = project(name);
        let mut sections: [Vec<Story>; 5] = Default::default();
        for title in titles {
            let story = Story::new(title.to_string(), String::new(), SendMode::Both, &dir);
            save_story(&dir, &story.name, &story.anchor.source(&story.prompt)).unwrap();
            sections[1].push(story);
        }
        save_order_text(&dir, &order_of(&sections, &[], &Default::default())).unwrap();
        let names = sections[1].iter().map(|story| story.name.clone()).collect();
        (dir, names)
    }

    fn titles(
        planning: &Entity<Planning>,
        section: Section,
        cx: &mut TestAppContext,
    ) -> Vec<String> {
        planning.read_with(cx, |planning, _| {
            planning
                .stories(section)
                .iter()
                .map(|story| story.title.clone())
                .collect()
        })
    }

    /// Stories, their order, which section each is in, their marks, and
    /// the proposals waiting come back as they were saved.
    #[test]
    fn the_plan_comes_back_as_it_was_left() {
        let dir = project("planning-save-test");
        let mut sections: [Vec<Story>; 5] = Default::default();
        let mut first = Story::new(
            "First".into(),
            "Do the first thing".into(),
            SendMode::Code,
            &dir,
        );
        first.anchor.sliced = true;
        let mut second = Story::new("Second".into(), String::new(), SendMode::Ask, &dir);
        second.from_agent = true;
        let mut third = Story::new("Third".into(), "Third".into(), SendMode::Spec, &dir);
        third.task = Some("ThirdTask".into());
        for story in [&first, &second, &third] {
            save_story(&dir, &story.name, &story.anchor.source(&story.prompt)).unwrap();
        }
        sections[0].push(first);
        sections[1].push(second);
        sections[2].push(third);
        let proposal =
            parse_story_block("title: Later\nmode: spec\ntarget: new\n\nLater.").unwrap();
        save_order_text(
            &dir,
            &order_of(
                &sections,
                std::slice::from_ref(&proposal),
                &Default::default(),
            ),
        )
        .unwrap();

        let (loaded, proposals, _) = load(&dir);
        let titles: Vec<Vec<&str>> = loaded
            .iter()
            .map(|stories| stories.iter().map(|story| story.title.as_str()).collect())
            .collect();
        assert_eq!(titles[..3], [["First"], ["Second"], ["Third"]]);
        assert!(titles[3].is_empty());
        assert_eq!(loaded[0][0].prompt, "Do the first thing");
        assert_eq!(loaded[0][0].mode(), SendMode::Code);
        assert!(loaded[0][0].anchor.sliced);
        assert!(loaded[1][0].from_agent);
        // A title with no prompt sends its title.
        assert_eq!(loaded[1][0].text(), "Second");
        assert_eq!(loaded[2][0].task.as_deref(), Some("ThirdTask"));
        assert_eq!(proposals, [proposal]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Draft is saved with the other sections; a plan saved before Draft
    /// existed loads with Draft empty, and is saved with one next time.
    #[test]
    fn draft_is_saved_and_an_old_plan_loads_with_it_empty() {
        let dir = project("planning-draft-save-test");
        let mut sections: [Vec<Story>; 5] = Default::default();
        let rough = Story::new("Rough".into(), String::new(), SendMode::Code, &dir);
        let ready = Story::new("Ready".into(), String::new(), SendMode::Code, &dir);
        for story in [&rough, &ready] {
            save_story(&dir, &story.name, &story.anchor.source(&story.prompt)).unwrap();
        }
        let (rough_name, ready_name) = (rough.name.clone(), ready.name.clone());
        sections[section_ix(Section::Draft)].push(rough);
        sections[section_ix(Section::Backlog)].push(ready);
        let text = order_of(&sections, &[], &Default::default());
        assert!(text.contains("\"draft\""));
        save_order_text(&dir, &text).unwrap();
        let (loaded, _, _) = load(&dir);
        assert_eq!(loaded[section_ix(Section::Draft)][0].title, "Rough");
        assert_eq!(loaded[section_ix(Section::Backlog)][0].title, "Ready");

        std::fs::remove_file(story_file(&dir, &rough_name)).unwrap();
        let old = serde_json::json!({
            "next": [],
            "backlog": [{ "story": ready_name, "title": "Ready" }],
            "done": [],
        });
        save_order_text(&dir, &old.to_string()).unwrap();
        let (loaded, _, _) = load_checked(&dir).unwrap();
        assert!(loaded[section_ix(Section::Draft)].is_empty());
        assert_eq!(loaded[section_ix(Section::Backlog)][0].title, "Ready");
        let saved = order_of(&loaded, &[], &Default::default());
        assert!(saved.contains("\"draft\": []"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Proposals are read from `suspense-story` blocks wherever they stand
    /// in a reply, each with its title, mode, target, and prompt.
    #[test]
    fn proposals_are_read_from_story_blocks() {
        let reply = "I read the queue.\n\n```suspense-story\ntitle: Add sprints\nmode: chain\ntarget: new\n\nStories can be gathered\ninto a sprint.\n```\n\nAnd:\n\n```suspense-story\ntitle: Test it\nmode: code\ntarget: Story_1\n\nCover the sprint.\n```\n";
        let proposals = parse_proposals(reply);
        assert_eq!(proposals.len(), 2);
        assert_eq!(proposals[0].title, "Add sprints");
        assert_eq!(proposals[0].mode(), SendMode::Both);
        assert_eq!(proposals[0].target, None);
        assert_eq!(
            proposals[0].prompt,
            "Stories can be gathered\ninto a sprint."
        );
        assert_eq!(proposals[1].mode(), SendMode::Code);
        assert_eq!(proposals[1].target.as_deref(), Some("Story_1"));
        assert!(parse_proposals("Nothing to propose.").is_empty());
    }

    /// A dragged card's gap is chosen in the column under the pointer, by
    /// the halves of its cards; an empty column takes it at its top; a
    /// column that takes no drops, and either side of the card itself,
    /// show none.
    #[test]
    fn a_dragged_card_finds_its_gap_in_the_column_under_it() {
        use gpui_kit::{Bounds, point, px, size};
        let column = |x: f32| {
            let bounds = Bounds::new(point(px(x), px(0.)), size(px(320.), px(600.)));
            (bounds, bounds)
        };
        let row = |x: f32, y: f32| Bounds::new(point(px(x), px(y)), size(px(320.), px(40.)));
        let columns_at: super::ColumnBounds = [
            ("backlog", column(0.)),
            ("next", column(336.)),
            ("current", column(672.)),
        ]
        .into_iter()
        .collect();
        let rows: std::collections::HashMap<String, _> = [
            ("a".to_string(), row(0., 40.)),
            ("b".to_string(), row(0., 90.)),
            ("c".to_string(), row(336., 40.)),
        ]
        .into_iter()
        .collect();
        let columns = vec![
            (Section::Backlog, vec!["a".to_string(), "b".to_string()]),
            (Section::Next, vec!["c".to_string()]),
        ];
        let gap = |name: &str, x: f32, y: f32| {
            super::choose_gap(
                name,
                &[],
                gpui_kit::point(px(x), px(y)),
                &columns,
                &columns_at,
                &rows,
            )
        };
        // Into the other column, over the first or second half of its card.
        assert_eq!(gap("a", 400., 50.), Some((Section::Next, 0)));
        assert_eq!(gap("a", 400., 70.), Some((Section::Next, 1)));
        // Past the last card, after it.
        assert_eq!(gap("c", 100., 500.), Some((Section::Backlog, 2)));
        // Either side of the card itself, none.
        assert_eq!(gap("a", 100., 10.), None);
        assert_eq!(gap("a", 100., 70.), None);
        assert_eq!(gap("b", 100., 50.), Some((Section::Backlog, 0)));
        // The lifted card is out of its column: where it was drawn never
        // counts, so over its old place the gap is its own, and none.
        assert_eq!(gap("a", 100., 45.), None);
        assert_eq!(gap("a", 100., 115.), Some((Section::Backlog, 2)));
        // Carrying the selection, its cards are all out of their column,
        // and even its own place gathers them there.
        let carrying = |x: f32, y: f32| {
            super::choose_gap(
                "a",
                &["b".to_string()],
                gpui_kit::point(px(x), px(y)),
                &columns,
                &columns_at,
                &rows,
            )
        };
        assert_eq!(carrying(100., 10.), Some((Section::Backlog, 0)));
        assert_eq!(carrying(100., 115.), Some((Section::Backlog, 2)));
        assert_eq!(carrying(400., 50.), Some((Section::Next, 0)));
        // Current sprint takes no drops, nor does anywhere off the columns.
        assert_eq!(gap("a", 700., 50.), None);
        assert_eq!(gap("a", 2000., 50.), None);
        // An empty column takes a drop at its top.
        let columns = vec![(Section::Next, Vec::new())];
        let at = point(px(400.), px(300.));
        assert_eq!(
            super::choose_gap("a", &[], at, &columns, &columns_at, &rows),
            Some((Section::Next, 0))
        );
    }

    /// Held within 32 pixels of an edge, or past it, an area scrolls toward
    /// it, faster the nearer or further past, up to a row each 50ms.
    #[test]
    fn a_card_held_near_an_edge_scrolls_its_area() {
        use gpui_kit::px;
        let speed = |at: f32| super::edge_speed(px(at), px(100.), px(500.));
        assert_eq!(speed(300.), 0.);
        assert_eq!(speed(132.), 0.);
        assert!(speed(120.) > 0. && speed(110.) > speed(120.));
        assert!(speed(480.) < 0. && speed(490.) < speed(480.));
        assert_eq!(speed(0.), super::DRAG_MOST_SPEED);
        assert_eq!(speed(900.), -super::DRAG_MOST_SPEED);
    }

    /// A column of a thousand stories lays out only the cards in view and a
    /// few either side, as the TableDrawing says of a table's rows, the rest
    /// measured over the following frames, so its scrollbar holds still once
    /// they are; and a story opened from outside the board, its card at the
    /// column's far end, is scrolled straight to, without drawing the cards
    /// between.
    #[gpui_kit::test]
    async fn a_long_column_draws_only_the_cards_in_view(cx: &mut TestAppContext) {
        use gpui_kit::{px, size};
        let titles: Vec<String> = (0..1000).map(|n| format!("Story {n}")).collect();
        let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
        let (dir, names) = backlog("planning-long-column-test", &titles);
        let (planning, handle) = open_planning(dir.clone(), unused, cx);
        cx.simulate_window_resize(handle, size(px(1400.), px(700.)));
        cx.run_until_parked();
        // A frame as the platform draws one, the animation frames asked
        // for delivered; how many of the Backlog's cards it drew.
        let frame = |cx: &mut TestAppContext| -> usize {
            cx.update_window(handle, |_, window, cx| {
                window.simulate_next_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx))
                .unwrap();
            planning.read_with(cx, |planning, _| {
                planning.column_cards["backlog"].borrow().drawn.len()
            })
        };
        let measured = |cx: &mut TestAppContext| {
            planning.read_with(cx, |planning, _| {
                let cards = planning.column_cards["backlog"].borrow();
                names.iter().filter(|name| cards.exact(name)).count()
            })
        };
        let mut most = 0;
        for _ in 0..200 {
            most = most.max(frame(cx));
            if measured(cx) == names.len() {
                break;
            }
        }
        assert!(most > 0, "the Backlog drew no cards");
        assert!(most < 40, "the Backlog drew {most} of its 1000 cards at once");
        assert_eq!(measured(cx), names.len(), "the cards out of view were never all measured");
        // Every card measured, its scrollbar holds still.
        let max = |cx: &mut TestAppContext| {
            planning.read_with(cx, |planning, _| planning.column_scroll["backlog"].max_offset().y)
        };
        frame(cx);
        let settled = max(cx);
        assert!(settled > px(10_000.), "the Backlog scrolls only {settled:?}");
        for _ in 0..3 {
            frame(cx);
            assert_eq!(max(cx), settled, "the Backlog's scrollbar moved with nothing changed");
        }

        // Its last story opened from outside the board: the column jumps
        // straight to its card, fully in view, drawing no more cards than
        // fit around it.
        let last = names.last().unwrap().clone();
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| planning.reveal_story(&last, window, cx));
        })
        .unwrap();
        let mut most = 0;
        for _ in 0..30 {
            most = most.max(frame(cx));
        }
        assert!(most < 40, "scrolling to the card drew {most} cards");
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let column = planning.read(cx).column_scroll["backlog"].bounds();
            let card = planning.read(cx).row_bounds.borrow().get(&last).copied();
            let card = card.expect("the opened story's card isn't drawn");
            assert!(
                card.top() >= column.top() - px(1.) && card.bottom() <= column.bottom() + px(1.),
                "the opened story's card {card:?} isn't in its column's view {column:?}"
            );
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A mouse's horizontal wheel, over a column or a gap between columns,
    /// scrolls the board sideways the way it is turned, never the column.
    #[gpui_kit::test]
    async fn the_horizontal_wheel_scrolls_the_board_sideways(cx: &mut TestAppContext) {
        use gpui_kit::{InputEvent as _, ScrollDelta, ScrollWheelEvent, point, px, size};
        let titles: Vec<String> = (0..40).map(|n| format!("Story {n}")).collect();
        let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
        let (dir, _) = backlog("planning-horizontal-wheel-test", &titles);
        let (planning, handle) = open_planning(dir.clone(), unused, cx);
        cx.simulate_window_resize(handle, size(px(900.), px(600.)));
        cx.run_until_parked();
        // A tilt or thumb wheel turned right, as Linux reports it: lines
        // along x alone, negative.
        let turn = |x: f32, at: gpui_kit::Point<gpui_kit::Pixels>, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.dispatch_event(
                    ScrollWheelEvent {
                        position: at,
                        delta: ScrollDelta::Lines(point(x, 0.)),
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        let scrolled = |cx: &mut TestAppContext| {
            planning.read_with(cx, |planning, _| {
                (
                    planning.board_scroll.offset().x,
                    planning.column_scroll["backlog"].offset().y,
                )
            })
        };
        let (column, next) = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.find("planning-column-backlog").bounds(),
                    window.find("planning-column-next").bounds(),
                )
            })
            .unwrap();
        let (left, right) = if column.left() < next.left() {
            (column, next)
        } else {
            (next, column)
        };
        assert!(
            planning.read_with(cx, |planning, _| {
                planning.column_scroll["backlog"].max_offset().y > px(0.)
            }),
            "the Backlog has nothing to scroll"
        );

        // Over a column, right moves the board right; the column stays.
        turn(-1., column.center(), cx);
        let (board, down) = scrolled(cx);
        assert!(board < px(0.), "the board didn't scroll: {board:?}");
        assert_eq!(down, px(0.), "the column scrolled");

        // Over the gap between two columns, the same.
        let gap = point((left.right() + right.left()) / 2., left.center().y);
        turn(-1., gap, cx);
        let (further, down) = scrolled(cx);
        assert!(further < board, "the board didn't scroll from the gap");
        assert_eq!(down, px(0.), "the column scrolled");

        // Left moves it back.
        turn(1., gap, cx);
        turn(1., column.center(), cx);
        let (back, down) = scrolled(cx);
        assert_eq!(back, px(0.));
        assert_eq!(down, px(0.));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_request_lists_the_stories_and_what_is_wanted() {
        let request = draft_request(
            "A sprint board",
            &[("Story_1".into(), "Queue".into())],
            None,
            ".claude/reference",
            "spec",
        );
        // The drafter is told to search the spec and cite what it found,
        // into the Piton source, each with its kind and its words.
        assert!(request.contains("search the project's spec"));
        assert!(request.contains("spec/scope/planning/index.pi#Planning.sprint"));
        assert!(request.contains("Requirement, Behaviour, Name, or Section"));
        assert!(!request.contains("<spec>"));
        assert!(request.contains("No match"));
        assert!(request.ends_with("A sprint board"));
        assert!(request.contains("- Story_1: Queue"));
        assert!(request.contains("```suspense-story"));
    }

    /// Empty, each section says what is missing and what to do.
    #[gpui_kit::test]
    async fn empty_sections_say_what_to_do(cx: &mut TestAppContext) {
        let dir = project("planning-empty-test");
        let (_, handle) = open_planning(dir.clone(), unused, cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            // Every column is there, empty or not.
            window.find("planning-empty-next");
            window.find("planning-empty-backlog");
            window.find("planning-empty-current");
            window.find("planning-empty-done");
            window.find("planning-empty-findings");
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A story is added in its list, as a card is in Trello: Ctrl+N on a
    /// Backlog card opens the composer at the Backlog's foot, Enter adds the
    /// story there, never opened, and opens another beneath it, empty Enter
    /// adds nothing, and Escape discards it, the focus going back to the
    /// "Add story" line. "+ Add" adds at the top of its list.
    #[gpui_kit::test]
    async fn stories_are_added_where_they_will_sit(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-composer-test", &["One", "Two"]);
        let (planning, handle) = open_planning(dir.clone(), unused, cx);
        let press = |key: &str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.press(key, cx);
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        let type_title = |text: &str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                let title = planning.read(cx).composer.as_ref().unwrap().title.clone();
                title.update(cx, |input, cx| input.set_value(text, window, cx));
            })
            .unwrap();
        };
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            planning.update(cx, |planning, cx| planning.focus_row(&names[0], window, cx));
        })
        .unwrap();
        press("ctrl-n", cx);
        cx.update_window(handle, |_, window, cx| {
            let planning = planning.read(cx);
            let composer = planning.composer.as_ref().expect("no composer opened");
            assert_eq!((composer.section, composer.at), (Section::Backlog, 2));
            assert!(composer.title.read(cx).focus_handle(cx).is_focused(window));
            window.find("planning-composer-backlog");
        })
        .unwrap();

        type_title("Three", cx);
        press("enter", cx);
        assert_eq!(
            titles(&planning, Section::Backlog, cx),
            ["One", "Two", "Three"]
        );
        planning.read_with(cx, |planning, _| {
            assert!(planning.open.is_none(), "the story added was opened");
            let composer = planning.composer.as_ref().expect("no composer beneath");
            assert_eq!((composer.section, composer.at), (Section::Backlog, 3));
        });
        // Empty, Enter adds nothing.
        press("enter", cx);
        assert_eq!(titles(&planning, Section::Backlog, cx).len(), 3);

        type_title("Never", cx);
        press("escape", cx);
        assert_eq!(titles(&planning, Section::Backlog, cx).len(), 3);
        cx.update_window(handle, |_, window, cx| {
            let planning = planning.read(cx);
            assert!(planning.composer.is_none());
            assert_eq!(
                planning.focused_row(window),
                Some(super::add_line(Section::Backlog))
            );
        })
        .unwrap();

        // "+ Add" on Next sprint's heading: at the top of the list.
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| {
                planning.add_story(Section::Next, "Later".into(), String::new(), SendMode::Code, cx);
                planning.compose_at_top(Section::Next, window, cx);
            });
        })
        .unwrap();
        type_title("First", cx);
        press("enter", cx);
        assert_eq!(titles(&planning, Section::Next, cx), ["First", "Later"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// From the keyboard: Down moves to the next row, Alt+Up moves a story
    /// up within its column, Alt+Right on into Next sprint, M opens the mode picker, picked from with
    /// the arrows and Enter, and Delete asks before Enter deletes.
    #[gpui_kit::test]
    async fn stories_are_worked_from_the_keyboard(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-keys-test", &["One", "Two", "Three"]);
        let (planning, handle) = open_planning(dir.clone(), unused, cx);
        let press = |key: &str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.press(key, cx);
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        let focused = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                planning.read(cx).focused_row(window)
            })
            .unwrap()
        };
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            planning.update(cx, |planning, cx| planning.focus_row(&names[0], window, cx));
        })
        .unwrap();
        press("down", cx);
        assert_eq!(focused(cx).as_deref(), Some(names[1].as_str()));

        press("alt-up", cx);
        assert_eq!(
            titles(&planning, Section::Backlog, cx),
            ["Two", "One", "Three"]
        );
        // At the top of its column, it stays there.
        press("alt-up", cx);
        assert_eq!(
            titles(&planning, Section::Backlog, cx),
            ["Two", "One", "Three"]
        );
        press("alt-right", cx);
        assert_eq!(titles(&planning, Section::Next, cx), ["Two"]);
        assert_eq!(focused(cx).as_deref(), Some(names[1].as_str()));

        // Ctrl+Enter moves it to the Backlog, and back: never sends it.
        press("ctrl-enter", cx);
        assert!(titles(&planning, Section::Next, cx).is_empty());
        assert_eq!(titles(&planning, Section::Backlog, cx)[0], "Two");
        press("ctrl-enter", cx);
        assert_eq!(titles(&planning, Section::Next, cx), ["Two"]);
        assert!(titles(&planning, Section::Current, cx).is_empty());

        let mode = |cx: &mut TestAppContext| {
            planning.read_with(cx, |planning, _| planning.story(&names[1]).unwrap().mode())
        };
        let before = mode(cx);
        press("m", cx);
        cx.update_window(handle, |_, window, _| {
            window.find(SharedString::from(format!(
                "planning-mode-menu-{}",
                names[1]
            )));
        })
        .unwrap();
        press("down", cx);
        press("enter", cx);
        assert_ne!(mode(cx), before, "the mode picked wasn't set");
        assert!(planning.read_with(cx, |planning, _| planning.mode_menu.is_none()));

        press("delete", cx);
        cx.update_window(handle, |_, window, _| {
            window.find(SharedString::from(format!("planning-delete-{}", names[1])));
        })
        .unwrap();
        assert_eq!(
            titles(&planning, Section::Next, cx),
            ["Two"],
            "deleted unasked"
        );
        press("enter", cx);
        assert!(titles(&planning, Section::Next, cx).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The story the drafting test's harness is asked to add to.
    static TARGET: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

    /// A spec file in the project at `dir`, under the spec location, that
    /// declares Planning and its sprint.
    fn reference(dir: &Path) {
        let page = dir.join("spec/scope/planning");
        std::fs::create_dir_all(&page).unwrap();
        std::fs::write(
            page.join("index.pi"),
            "// The plan.\nexport scope Planning:\n    sprint:\n        Sent in order.\n        note: not a property\n    layout: Rows\n",
        )
        .unwrap();
    }

    /// A spec list's entries are read as links with what they touch, or as
    /// No match with what was looked for; a link counts only where its
    /// heading exists in the compiled reference.
    #[test]
    fn citations_are_read_and_checked() {
        let dir = project("planning-cite-test");
        reference(&dir);
        let block = "title: Sprint\nmode: chain\ntarget: new\nspec:\n- Behaviour spec/scope/planning/index.pi#Planning.sprint — how it is sent — \"Sent in order.\"\n- [Planning](spec/scope/planning/index.pi#Planning)\n- No match — a calendar\n\nDo it.";
        let proposal = parse_story_block(block).unwrap();
        assert_eq!(proposal.prompt, "Do it.");
        assert_eq!(proposal.spec.len(), 3);
        assert_eq!(proposal.spec[0].note, "how it is sent");
        assert_eq!(proposal.spec[0].kind, Some(CitationKind::Behaviour));
        assert_eq!(proposal.spec[0].quote, "Sent in order.");
        assert_eq!(proposal.spec[0].label(), "Planning.sprint");
        assert_eq!(
            proposal.spec[0].file(),
            Some("spec/scope/planning/index.pi")
        );
        assert_eq!(proposal.spec[1].kind, None);
        assert_eq!(
            proposal.spec[1].link.as_deref(),
            Some("spec/scope/planning/index.pi#Planning")
        );
        assert_eq!(proposal.spec[2].link, None);
        assert_eq!(proposal.spec[2].note, "a calendar");
        // The example the planning prompt explains the form with reads back
        // as it says.
        let example = parse_proposals(&story_example("spec"));
        assert_eq!(example.len(), 1);
        assert_eq!(example[0].title, "Close a finished sprint");
        assert_eq!(
            example[0].spec[0].link.as_deref(),
            Some("spec/scope/planning/index.pi#Planning.sprint")
        );
        assert_eq!(example[0].spec[0].note, "when Close sprint shows");
        assert_eq!(example[0].spec[1].link, None);
        assert_eq!(example[0].spec[1].note, "archiving a closed sprint");
        assert!(example[0].prompt.starts_with("Close sprint is offered"));
        // Checked against the Piton source, found at its declaration.
        assert_eq!(
            super::check_link("spec/scope/planning/index.pi#Planning.sprint", &dir),
            Ok(Some(2))
        );
        assert_eq!(
            super::check_link("spec/scope/planning/index.pi#Planning", &dir),
            Ok(Some(1))
        );
        assert_eq!(
            super::check_link("spec/scope/planning/index.pi#Planning.layout", &dir),
            Ok(Some(5))
        );
        // A property's text is no property of it.
        assert_eq!(
            super::check_link("spec/scope/planning/index.pi#Planning.note", &dir),
            Err("the file doesn't define Planning.note".to_string())
        );
        assert_eq!(
            super::check_link("spec/scope/planning/index.pi#Planning.nowhere", &dir),
            Err("the file doesn't define Planning.nowhere".to_string())
        );
        assert_eq!(
            super::check_link("spec/scope/gone/index.pi#Gone.sprint", &dir),
            Err("no such file".to_string())
        );
        // Never the compiled reference, nor outside the spec location.
        assert!(
            super::check_link(".claude/reference/scope/planning/index.md#sprint", &dir).is_err()
        );
        assert!(super::check_link("spec/../spec/scope/planning/index.pi#Planning", &dir).is_err());
        // Add citation finds the spec's elements by name.
        let elements = super::spec_elements(&dir);
        let picks = super::citation_picks(&elements, "sprint");
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].0, "spec/scope/planning/index.pi#Planning.sprint");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A proposal citing nothing, not even No match, isn't a story: it can't
    /// be accepted, and its card offers Draft again.
    #[gpui_kit::test]
    async fn a_proposal_citing_no_spec_is_not_a_story(cx: &mut TestAppContext) {
        let dir = project("planning-ungrounded-test");
        let (planning, handle) = open_planning(dir.clone(), unused, cx);
        let proposal = parse_story_block("title: Bare\nmode: code\n\nJust do it.").unwrap();
        let id = proposal.id.clone();
        planning.update(cx, |planning, cx| {
            planning.proposals.push(proposal);
            assert!(planning.accept(&id, cx).is_err());
            assert!(planning.stories(Section::Next).is_empty());
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find(SharedString::from(format!("planning-draft-again-{id}")));
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    fn two_proposals(
        _: &Path,
        request: &str,
        _: &Cancel,
        on_line: &dyn Fn(String),
    ) -> anyhow::Result<String> {
        on_line(r#"{"type":"assistant"}"#.into());
        let target = TARGET.lock().unwrap().clone();
        assert!(
            request.contains(&target),
            "the request doesn't name the stories"
        );
        Ok(format!(
            "Here:\n\n```suspense-story\ntitle: First\nmode: code\ntarget: new\nspec:\n- spec/scope/planning/index.pi#Planning.sprint — how it is sent\n- spec/scope/planning/index.pi#Planning.nowhere — made up\n\nDo the first\n```\n\n```suspense-story\ntitle: More\nmode: spec\ntarget: {target}\nspec:\n- No match — a calendar\n\nDescribe more\n```\n"
        ))
    }

    /// Enter in the request box starts the draft, as Draft does, and only
    /// that: an empty request starts nothing, and the list's Enter never
    /// fires there.
    #[gpui_kit::test]
    async fn enter_in_the_request_box_starts_the_draft(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-draft-enter-test", &["Queue"]);
        reference(&dir);
        *TARGET.lock().unwrap() = names[0].clone();
        let (planning, handle) = open_planning(dir.clone(), two_proposals, cx);
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| {
                planning.open_draft_panel(None, true, window, cx)
            });
        })
        .unwrap();
        cx.run_until_parked();
        // Empty, Enter starts nothing, and Draft is disabled.
        cx.update_window(handle, |_, window, cx| window.press("enter", cx))
            .unwrap();
        cx.run_until_parked();
        assert!(planning.read_with(cx, |planning, _| planning.proposals.is_empty()));
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| {
                let input = planning.drafting.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| input.set_value("Two things", window, cx));
            });
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            planning.read_with(cx, |planning, _| planning.proposals.len()),
            2
        );
        // Only the request was sent: no story was opened by the list's Enter.
        assert!(planning.read_with(cx, |planning, _| planning.open.is_none()));
        // Shift+Enter adds a line and sends nothing.
        let value = |cx: &mut TestAppContext| {
            planning.read_with(cx, |planning, cx| {
                planning
                    .drafting
                    .as_ref()
                    .unwrap()
                    .input
                    .read(cx)
                    .value()
                    .to_string()
            })
        };
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| {
                let input = planning.drafting.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| {
                    input.set_value("More", window, cx);
                    input.focus(window, cx);
                });
            });
            window.press("shift-enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(value(cx), "More\n");
        assert!(planning.read_with(cx, |planning, _| {
            planning.drafting.as_ref().unwrap().running.is_none() && planning.proposals.len() == 2
        }));
        // While a run is going, Enter starts no second one.
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, _| {
                planning.drafting.as_mut().unwrap().running = Some(std::time::Instant::now());
            });
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(planning.read_with(cx, |planning, _| planning.proposals.len() == 2));
        // Ctrl+Enter starts it too.
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, _| {
                planning.drafting.as_mut().unwrap().running = None;
            });
            window.press("secondary-enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            planning.read_with(cx, |planning, _| planning.proposals.len()),
            4
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The agent only proposes: nothing reaches the plan until accepted.
    /// Proposals are kept with the project; accepting a new story puts it at
    /// the Backlog's end from the agent, and an addition becomes a step of
    /// the story it names, marked as updated; once none is left, the panel
    /// closes.
    #[gpui_kit::test]
    async fn proposals_reach_the_plan_only_once_accepted(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-propose-test", &["Queue"]);
        reference(&dir);
        *TARGET.lock().unwrap() = names[0].clone();
        let (planning, handle) = open_planning(dir.clone(), two_proposals, cx);
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| {
                planning.open_draft_panel(None, true, window, cx);
                let input = planning.drafting.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| input.set_value("Two things", window, cx));
                planning.draft(cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        let ids: Vec<String> = planning.read_with(cx, |planning, _| {
            planning
                .proposals
                .iter()
                .map(|proposal| proposal.id.clone())
                .collect()
        });
        assert_eq!(ids.len(), 2);
        // Nothing written to the plan yet, only proposed, and kept.
        assert_eq!(titles(&planning, Section::Backlog, cx), ["Queue"]);
        let (saved, proposals, _) = load(&dir);
        assert_eq!(saved[1].len(), 1);
        assert_eq!(proposals.len(), 2);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find(SharedString::from(format!("planning-proposal-{}", ids[0])));
            window.find("planning-accept-all");
        })
        .unwrap();

        // A accepts the focused one; the other, an addition, too.
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| planning.focus_row(&ids[0], window, cx));
            window.render_frame(cx);
            window.press("a", cx);
        })
        .unwrap();
        cx.run_until_parked();
        // A new story goes to the end of the Backlog, never Next sprint.
        assert!(titles(&planning, Section::Next, cx).is_empty());
        assert_eq!(titles(&planning, Section::Backlog, cx), ["Queue", "First"]);
        planning
            .update(cx, |planning, cx| planning.accept(&ids[1], cx))
            .unwrap();
        planning.read_with(cx, |planning, _| {
            let first = &planning.stories(Section::Backlog)[1];
            let links: Vec<Option<&str>> = first
                .spec
                .iter()
                .map(|citation| citation.link.as_deref())
                .collect();
            assert_eq!(
                links,
                [Some("spec/scope/planning/index.pi#Planning.sprint")]
            );
            assert!(first.from_agent);
            assert_eq!(first.mode(), SendMode::Code);
            let queue = &planning.stories(Section::Backlog)[0];
            assert!(queue.updated);
            assert_eq!(queue.steps().len(), 1);
            assert_eq!(queue.steps()[0].prompt, "Describe more");
            assert_eq!(queue.steps()[0].mode, SendMode::Spec);
            assert!(planning.proposals.is_empty());
            assert!(planning.drafting.is_none(), "the panel didn't close");
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An addition to a story gone by the time it is accepted adds nothing:
    /// the card stays, made a new story, saying why. A rejected proposal
    /// can be brought back with Undo.
    #[gpui_kit::test]
    async fn additions_to_a_story_gone_are_refused_and_rejects_undone(cx: &mut TestAppContext) {
        let dir = project("planning-refuse-test");
        let (planning, handle) = open_planning(dir.clone(), unused, cx);
        let proposal = Proposal {
            target: Some("Story_gone".into()),
            ..parse_story_block("title: More\nmode: code\nspec:\n- No match — more\n\nMore.")
                .unwrap()
        };
        let id = proposal.id.clone();
        planning.update(cx, |planning, cx| {
            planning.proposals.push(proposal);
            assert!(planning.accept(&id, cx).is_err());
            let kept = planning.proposal(&id).unwrap();
            assert_eq!(kept.target, None);
            assert!(planning.stories(Section::Backlog).is_empty());
            planning.reject(&id, cx);
            assert!(planning.proposals.is_empty());
            planning.undo_reject(&id, cx);
            assert_eq!(planning.proposals.len(), 1);
        });
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Steps are added at the end of a story, in order, saved in its own
    /// file, kept when the story moves and copied when it is duplicated; one
    /// added to a story that doesn't exist, or whose file is gone, or that
    /// was sent, changes nothing and says why.
    #[gpui_kit::test]
    async fn steps_are_kept_with_their_story(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-steps-test", &["Queue", "Gone"]);
        let (planning, handle) = open_planning(dir.clone(), unused, cx);
        planning.update(cx, |planning, cx| {
            assert_eq!(
                planning.add_step(&names[0], SendMode::Code, "Two".into(), cx),
                Ok(0)
            );
            assert_eq!(
                planning.add_step(&names[0], SendMode::Spec, "Three".into(), cx),
                Ok(1)
            );
            planning.move_step(&names[0], 1, 0, cx);
            planning.move_to(&names[0], Section::Next, cx);
            planning.duplicate(&names[0], cx);
        });
        cx.run_until_parked();
        let (saved, _, _) = load(&dir);
        let prompts: Vec<&str> = saved[0][0]
            .steps()
            .iter()
            .map(|step| step.prompt.as_str())
            .collect();
        assert_eq!(prompts, ["Three", "Two"]);
        assert_eq!(saved[0][0].steps()[0].mode, SendMode::Spec);
        assert_eq!(saved[0][1].steps().len(), 2, "the duplicate lost its steps");

        // A story whose file is gone, or that doesn't exist, takes nothing.
        std::fs::remove_file(
            dir.join(".suspense/planning")
                .join(format!("{}.pi", names[1])),
        )
        .unwrap();
        planning.update(cx, |planning, cx| {
            let gone = planning.add_step(&names[1], SendMode::Code, "More".into(), cx);
            assert_eq!(gone, Err(super::NO_SUCH_STORY.to_string()));
            assert!(planning.story(&names[1]).unwrap().steps().is_empty());
            let missing = planning.add_step("Story_none", SendMode::Code, "More".into(), cx);
            assert_eq!(missing, Err(super::NO_SUCH_STORY.to_string()));
            planning.story_mut(&names[0]).unwrap().task = Some("Sent".into());
            let sent = planning.add_step(&names[0], SendMode::Code, "More".into(), cx);
            assert_eq!(sent, Err(super::STORY_SENT.to_string()));
            assert_eq!(planning.story(&names[0]).unwrap().steps().len(), 2);
        });
        cx.run_until_parked();
        assert!(
            !dir.join(".suspense/planning")
                .join(format!("{}.pi", names[1]))
                .exists()
        );
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Add step opens one at the end, its prompt focused; Enter in it once
    /// it holds something starts another, and Enter in an empty one adds
    /// none.
    #[gpui_kit::test]
    async fn steps_are_added_one_after_another(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-add-steps-test", &["Queue"]);
        let (planning, handle) = open_planning(dir.clone(), unused, cx);
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| {
                planning.open_story(&names[0], window, cx)
            });
            window.render_frame(cx);
            window.click(
                SharedString::from(format!("planning-add-step-{}", names[0])),
                cx,
            );
            window.render_frame(cx);
            let prompt = planning.read(cx).open_step.as_ref().unwrap().prompt.clone();
            prompt.update(cx, |input, cx| input.set_value("First step", window, cx));
            window.press("enter", cx);
            window.render_frame(cx);
            window.press("enter", cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        planning.read_with(cx, |planning, _| {
            let steps = planning.story(&names[0]).unwrap().steps();
            let prompts: Vec<&str> = steps.iter().map(|step| step.prompt.as_str()).collect();
            assert_eq!(prompts, ["First step"]);
            assert!(planning.open_step.is_none());
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    fn findings(_: &Path, request: &str, _: &Cancel, _: &dyn Fn(String)) -> anyhow::Result<String> {
        assert!(request.contains("change nothing"));
        Ok("Found the queue in `src/prompt_queue.rs:40`.\n\n```suspense-story\ntitle: Tidy it\nmode: code\ntarget: new\n\nTidy the queue.\n```\n".into())
    }

    /// Research ends in findings saved as markdown and listed, its proposal
    /// waiting, not added; and planning pauses for review: Send sprint does
    /// nothing until it is resumed, and the pause is kept across a reload.
    #[gpui_kit::test]
    async fn research_pauses_planning_until_resumed(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-research-test", &["Queue"]);
        let (planning, handle) = open_planning(dir.clone(), findings, cx);
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| {
                planning.move_to(&names[0], Section::Next, cx);
                planning.open_research(None, window, cx);
                let input = planning.drafting.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| input.set_value("The queue", window, cx));
                planning.draft(cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        let file = planning.read_with(cx, |planning, _| {
            assert!(planning.is_paused());
            assert_eq!(planning.notes.findings.len(), 1);
            assert_eq!(planning.notes.findings[0].state, super::FindingState::Done);
            // Proposed, never added by itself.
            assert_eq!(planning.proposals.len(), 1);
            assert_eq!(planning.stories(Section::Backlog).len(), 0);
            planning.notes.findings[0].file.clone()
        });
        let text = std::fs::read_to_string(super::research_dir(&dir).join(&file)).unwrap();
        assert!(text.starts_with("# Research: The queue"));
        assert!(text.contains("src/prompt_queue.rs:40"));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("planning-paused");
            window.find(SharedString::from(format!("planning-findings-{file}")));
            // Paused, nothing is sent.
            planning.update(cx, |planning, cx| planning.send_sprint(window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(titles(&planning, Section::Next, cx), ["Queue"]);
        assert!(titles(&planning, Section::Current, cx).is_empty());
        // Kept across a reload.
        let (_, _, notes) = load(&dir);
        assert_eq!(
            notes.paused.as_ref().map(|pause| pause.research.as_str()),
            Some(file.as_str())
        );
        // Resumed, the sprint can be sent, into Current sprint.
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| {
                planning.resume_planning(cx);
                planning.send_sprint(window, cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(titles(&planning, Section::Current, cx), ["Queue"]);
        assert!(titles(&planning, Section::Next, cx).is_empty());
        let (_, _, notes) = load(&dir);
        assert!(notes.paused.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// With Start automatically on, Next sprint is sent by itself: at once
    /// while no sprint is current, and once every story of the current one
    /// is Done, which closes it; never after a sprint with a story not done,
    /// saying why, nor while paused. The switch is kept with the plan.
    #[gpui_kit::test]
    async fn the_next_sprint_starts_by_itself(cx: &mut TestAppContext) {
        use std::cell::RefCell;
        use std::collections::HashMap;
        let (dir, names) = backlog("planning-auto-start-test", &["One", "Two", "Three"]);
        let states: Rc<RefCell<HashMap<String, super::StoryState>>> = Rc::default();
        let sent: Rc<RefCell<Vec<String>>> = Rc::default();
        let mut with = hooks(unused);
        with.state = Rc::new({
            let states = states.clone();
            move |task, _| states.borrow().get(task).map(|state| (*state, None))
        });
        with.send = Rc::new({
            let sent = sent.clone();
            move |stories, _| {
                sent.borrow_mut()
                    .extend(stories.into_iter().map(|story| story.text))
            }
        });
        let (planning, handle) = open_planning_with(dir.clone(), with, cx);
        planning.update(cx, |planning, cx| {
            planning.move_to(&names[0], Section::Next, cx);
            planning.set_auto_start(true, cx);
        });
        cx.run_until_parked();
        assert_eq!(*sent.borrow(), ["One"]);
        assert_eq!(titles(&planning, Section::Current, cx), ["One"]);
        // A sprint under way: the next waits for it.
        planning.update(cx, |planning, cx| {
            planning.move_to(&names[1], Section::Next, cx)
        });
        cx.run_until_parked();
        assert_eq!(titles(&planning, Section::Next, cx), ["Two"]);
        // Done, it closes, and the next is sent.
        let task = |planning: &Entity<Planning>, name: &str, cx: &mut TestAppContext| {
            planning.read_with(cx, |planning, _| {
                planning.story(name).unwrap().task.clone().unwrap()
            })
        };
        let one = task(&planning, &names[0], cx);
        states.borrow_mut().insert(one, super::StoryState::Done);
        planning.update(cx, |planning, cx| planning.auto_advance(cx));
        cx.run_until_parked();
        assert_eq!(titles(&planning, Section::Done, cx), ["One"]);
        assert_eq!(titles(&planning, Section::Current, cx), ["Two"]);
        assert_eq!(*sent.borrow(), ["One", "Two"]);
        // Failed, the next waits, saying why.
        let two = task(&planning, &names[1], cx);
        states.borrow_mut().insert(two, super::StoryState::Failed);
        planning.update(cx, |planning, cx| {
            planning.move_to(&names[2], Section::Next, cx)
        });
        cx.run_until_parked();
        assert_eq!(titles(&planning, Section::Current, cx), ["Two"]);
        assert_eq!(titles(&planning, Section::Next, cx), ["Three"]);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("planning-auto-start-held");
        })
        .unwrap();
        // Kept with the plan.
        let (_, _, notes) = load(&dir);
        assert!(notes.auto_start);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A finished sprint can always be closed: a story whose task can't be
    /// found has ended, not Done, reads "Task missing", and goes back to the
    /// start of Next sprint as it closes; an empty story is never sent, and
    /// stays in Next sprint, saying so. Closing is saved.
    #[gpui_kit::test]
    async fn a_sprint_with_a_missing_task_can_be_closed(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-missing-task-test", &["One"]);
        let (planning, handle) = open_planning(dir.clone(), unused, cx);
        planning.update(cx, |planning, cx| {
            planning.move_to(&names[0], Section::Next, cx);
            planning.add_story(
                Section::Next,
                String::new(),
                String::new(),
                SendMode::Code,
                cx,
            );
            assert_eq!(planning.start_sprint(cx), 1);
        });
        cx.run_until_parked();
        assert_eq!(titles(&planning, Section::Current, cx), ["One"]);
        assert_eq!(titles(&planning, Section::Next, cx).len(), 1);
        planning.read_with(cx, |planning, cx| {
            let story = planning.story(&names[0]).unwrap();
            assert_eq!(planning.state(story, cx), Some(StoryState::Missing));
            assert!(planning.sprint_ended(cx));
            assert!(planning.auto_held(cx).is_none());
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("planning-close-sprint");
            window.find("planning-task-missing");
            window.find("planning-left-empty");
        })
        .unwrap();
        planning.update(cx, |planning, cx| planning.close_sprint(cx));
        cx.run_until_parked();
        assert!(titles(&planning, Section::Current, cx).is_empty());
        assert!(titles(&planning, Section::Done, cx).is_empty());
        assert_eq!(titles(&planning, Section::Next, cx)[0], "One");
        // Saved as closed, the story sent again with the next sprint.
        let (saved, _, _) = load(&dir);
        assert!(saved[section_ix(Section::Current)].is_empty());
        assert_eq!(saved[section_ix(Section::Next)].len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Opened with Start automatically on and Next sprint waiting, nothing
    /// starts by itself: the switch's row says "Waiting to start", and Start
    /// now sends it.
    #[gpui_kit::test]
    async fn a_waiting_sprint_never_starts_as_the_plan_opens(cx: &mut TestAppContext) {
        use std::cell::RefCell;
        let (dir, names) = backlog("planning-auto-start-open-test", &["One"]);
        let (planning, _) = open_planning(dir.clone(), unused, cx);
        planning.update(cx, |planning, cx| {
            planning.move_to(&names[0], Section::Next, cx);
            // Saved on, as left, without starting it here.
            planning.advancing = true;
            planning.notes.auto_start = true;
            planning.save_order(cx);
            planning.advancing = false;
        });
        cx.run_until_parked();
        let sent: Rc<RefCell<Vec<String>>> = Rc::default();
        let mut with = hooks(unused);
        with.send = Rc::new({
            let sent = sent.clone();
            move |stories, _| {
                sent.borrow_mut()
                    .extend(stories.into_iter().map(|story| story.text))
            }
        });
        let (planning, handle) = open_planning_with(dir.clone(), with, cx);
        cx.run_until_parked();
        planning.update(cx, |planning, cx| planning.auto_advance(cx));
        cx.run_until_parked();
        assert!(sent.borrow().is_empty());
        assert_eq!(titles(&planning, Section::Next, cx), ["One"]);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("planning-auto-start-waiting");
            window.find("planning-auto-badge");
        })
        .unwrap();
        planning.update(cx, |planning, cx| planning.start_now(cx));
        cx.run_until_parked();
        assert_eq!(*sent.borrow(), ["One"]);
        assert_eq!(titles(&planning, Section::Current, cx), ["One"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The states and keys of the polish: an unreadable plan names its file
    /// with Try again and saves nothing over it; Findings says it has none;
    /// Ctrl+Shift+D opens Draft stories focused; F6 moves between regions;
    /// and Space toggles Start automatically from the keyboard.
    #[gpui_kit::test]
    async fn states_and_keys_of_the_plan(cx: &mut TestAppContext) {
        let dir = project("planning-polish-test");
        let order = super::planning_dir(&dir).join("order.json");
        std::fs::create_dir_all(order.parent().unwrap()).unwrap();
        std::fs::write(&order, "{ not json").unwrap();
        let (planning, handle) = open_planning(dir.clone(), unused, cx);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("planning-load-error");
            window.find("planning-load-retry");
        })
        .unwrap();
        // Nothing is saved over a plan that couldn't be read.
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| planning.new_story(window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(std::fs::read_to_string(&order).unwrap(), "{ not json");
        // Read again once it is fixed, what was added meanwhile kept.
        std::fs::write(&order, "{}").unwrap();
        planning.update(cx, |planning, cx| planning.read_again(cx));
        cx.run_until_parked();
        let press = |key: &str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.press(key, cx);
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("planning-empty-findings");
            planning.update(cx, |planning, cx| {
                planning.close_story(window, cx);
                planning.focus.focus(window, cx);
            });
        })
        .unwrap();
        press("ctrl-shift-d", cx);
        cx.update_window(handle, |_, window, cx| {
            let drafting = planning
                .read(cx)
                .drafting
                .as_ref()
                .expect("Draft stories didn't open");
            assert!(!drafting.research);
            assert!(drafting.input.read(cx).focus_handle(cx).is_focused(window));
        })
        .unwrap();
        // F6 moves on from the panel to the first region, and on again.
        press("f6", cx);
        let moved = cx
            .update_window(handle, |_, window, cx| {
                planning.read(cx).focused_row(window)
            })
            .unwrap();
        assert!(
            moved.is_some()
                || cx
                    .update_window(handle, |_, window, cx| {
                        let planning = planning.read(cx);
                        ["draft", "backlog", "next"]
                            .into_iter()
                            .any(|key| planning.section_focus[key].is_focused(window))
                    })
                    .unwrap()
        );
        // Space toggles the switch focused.
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| planning.switch_focus.focus(window, cx));
        })
        .unwrap();
        press("space", cx);
        assert!(planning.read_with(cx, |planning, _| planning.notes.auto_start));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn failing(_: &Path, _: &str, _: &Cancel, _: &dyn Fn(String)) -> anyhow::Result<String> {
        anyhow::bail!("the harness isn't signed in")
    }

    /// A run that fails says so, with Try again, and keeps the request.
    #[gpui_kit::test]
    async fn a_failed_draft_says_why_and_keeps_the_request(cx: &mut TestAppContext) {
        let dir = project("planning-draft-error-test");
        let (planning, handle) = open_planning(dir.clone(), failing, cx);
        cx.update_window(handle, |_, window, cx| {
            planning.update(cx, |planning, cx| {
                planning.open_draft_panel(None, true, window, cx);
                let input = planning.drafting.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| input.set_value("Something", window, cx));
                planning.draft(cx);
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.find("planning-draft-error");
            window.find("planning-draft-retry");
            let drafting = planning.read(cx).drafting.as_ref().unwrap();
            assert_eq!(drafting.input.read(cx).value().as_ref(), "Something");
            assert!(drafting.error.as_deref().unwrap().contains("signed in"));
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A draft of one story: the story in full, a new story, and its
    /// follow-up step.
    fn full_story(
        _: &Path,
        request: &str,
        _: &Cancel,
        _: &dyn Fn(String),
    ) -> anyhow::Result<String> {
        use crate::baked_prompts::planning;
        assert!(request.contains(planning::STORY), "it isn't asked for a full story");
        assert!(
            request.lines().any(|line| line.starts_with(planning::ABOUT)),
            "it isn't told the story"
        );
        let title = request
            .lines()
            .find_map(|line| line.strip_prefix("Title: "))
            .expect("it isn't told the title");
        assert!(request.contains("Mode: chain"), "it isn't told the mode");
        Ok(format!(
            "```suspense-story\ntitle: {title}, in full\nmode: code\ntarget: new\nspec:\n- No match — {title}\n\nAll of {title}.\n```\n\n```suspense-story\ntitle: Then\nmode: spec\ntarget: step\nspec:\n- No match — after\n\nThen {title}.\n```\n"
        ))
    }

    /// A block can be a follow-up step, and one saved as a rewrite names no
    /// story.
    #[test]
    fn a_block_can_follow_up_a_story() {
        let step = parse_story_block("title: Then\nmode: spec\ntarget: step\n\nSettle it.").unwrap();
        assert!(step.follow_up);
        assert_eq!(step.target, None);
        let proposal =
            parse_story_block("title: Whole\nmode: code\ntarget: rewrite\nspec:\n- No match — it\n\nAll of it.")
                .unwrap();
        assert!(proposal.rewrite);
        assert_eq!(proposal.target, None);
    }

    /// A draft about a story proposes only new stories: one naming the
    /// story drafted, or `rewrite`, is new; a step for it follows the new
    /// story before it, or, with none before it, is a new story itself.
    #[test]
    fn a_draft_about_a_story_proposes_only_new_stories() {
        let block = |text: &str| parse_story_block(text).unwrap();
        let folded = super::draft_proposals(
            "Story_a",
            vec![
                block("title: Lone\nmode: spec\ntarget: Story_a\n\nAlone."),
                block("title: Whole\nmode: chain\ntarget: rewrite\n\nAll of it."),
                block("title: Then\nmode: spec\ntarget: step\n\nSettle it."),
                block("title: Same\nmode: chain\ntarget: Story_a\n\nAgain."),
                block("title: Apart\nmode: chain\ntarget: Story_b\n\nElse."),
            ],
        );
        let titles: Vec<&str> = folded.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["Lone", "Whole", "Same", "Apart"]);
        assert!(folded.iter().all(|p| p.target.is_none() && p.drafted && !p.rewrite));
        assert_eq!(folded[0].step, None);
        let step = folded[1].step.as_ref().unwrap();
        assert_eq!((step.mode(), step.prompt.as_str()), (SendMode::Spec, "Settle it."));
    }

    /// Draft with agent drafts each story in a run of its own, all at
    /// once, never taking over the side panel. A story in the Backlog has
    /// what it proposes wait for it, saved a file per story, its follow-up
    /// step part of the new story it follows; accepting adds that story,
    /// with its step, at the Backlog's end, the story drafted left as it
    /// was. A story in Draft has its new story created straight away, as
    /// accepting would, and is marked "Drafted", kept in its file.
    #[gpui_kit::test]
    async fn stories_draft_with_the_agent_each_on_their_own(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-draft-with-agent-test", &["Rough", "Other"]);
        let (planning, _) = open_planning(dir.clone(), full_story, cx);
        planning.update(cx, |planning, cx| {
            planning.move_to(&names[0], Section::Draft, cx);
            planning.draft_with_agent(&names, cx);
            assert!(planning.story_drafting(&names[0]) && planning.story_drafting(&names[1]));
        });
        cx.run_until_parked();
        let file = super::draft_file(&dir, &names[1]);
        planning.update(cx, |planning, _| {
            assert!(planning.drafting.is_none() && planning.proposals.is_empty());
            assert!(!planning.story_drafting(&names[0]) && !planning.story_drafting(&names[1]));
            // In Draft: created, nothing to accept.
            assert!(planning.waiting(&names[0]).is_empty());
            let story = planning
                .stories(Section::Backlog)
                .iter()
                .find(|story| story.title == "Rough, in full")
                .expect("the Draft story's proposal wasn't created");
            assert_eq!(story.prompt, "All of Rough.");
            assert_eq!(story.mode(), SendMode::Code);
            assert!(story.from_agent);
            assert_eq!(story.steps().len(), 1);
            assert_eq!(story.steps()[0].mode, SendMode::Spec);
            let drafted = planning.story(&names[0]).unwrap();
            assert_eq!(drafted.title, "Rough");
            assert_eq!(drafted.drafted, 1);
            assert!(!drafted.updated && drafted.steps().is_empty());
            assert_eq!(planning.find(&names[0]).unwrap().0, Section::Draft);
            // In the Backlog: waiting.
            assert_eq!(planning.waiting(&names[1]).len(), 1);
            assert_eq!(planning.story(&names[1]).unwrap().drafted, 0);
        });
        assert!(file.exists(), "its proposals weren't saved");
        let (sections, _, notes) = load(&dir);
        assert_eq!(notes.drafts[&names[1]].proposals.len(), 1);
        assert!(!notes.drafts.contains_key(&names[0]));
        let reloaded = sections[section_ix(Section::Draft)]
            .iter()
            .find(|story| story.name == names[0])
            .unwrap();
        assert_eq!(reloaded.drafted, 1, "its mark wasn't kept in its file");
        planning.update(cx, |planning, cx| {
            let ids = planning.proposal_ids(Some(&names[1]));
            let added = planning.accept(&ids[0], cx).unwrap();
            assert_ne!(added, names[1]);
            let story = planning.story(&added).unwrap();
            assert_eq!(story.title, "Other, in full");
            assert!(story.from_agent);
            let backlog = planning.stories(Section::Backlog);
            assert_eq!(backlog.last().unwrap().name, added);
            assert!(planning.waiting(&names[1]).is_empty());
            assert_eq!(planning.story(&names[1]).unwrap().drafted, 0);
            // Moved, it keeps its mark.
            planning.move_to(&names[0], Section::Backlog, cx);
            assert_eq!(planning.story(&names[0]).unwrap().drafted, 1);
            // A story deleted takes its waiting proposals with it.
            planning.delete(&names[1], cx);
        });
        cx.run_until_parked();
        assert!(!file.exists(), "decided proposals were kept");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A draft of one story: a new story citing the spec, and one citing
    /// nothing, not even No match.
    fn half_grounded(
        _: &Path,
        _: &str,
        _: &Cancel,
        _: &dyn Fn(String),
    ) -> anyhow::Result<String> {
        Ok("```suspense-story\ntitle: Grounded\nmode: code\ntarget: new\nspec:\n- No match — it\n\nDo it.\n```\n\n```suspense-story\ntitle: Ungrounded\nmode: code\ntarget: new\n\nDo more.\n```\n".into())
    }

    /// A story in Draft drafted with the agent has only what passes the
    /// spec grounding created; what fails it waits on its card, to be
    /// drafted again. A run that fails leaves it unmarked.
    #[gpui_kit::test]
    async fn a_draft_story_creates_only_grounded_stories(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-draft-grounded-test", &["Rough", "Failing"]);
        let (planning, _) = open_planning(dir.clone(), half_grounded, cx);
        planning.update(cx, |planning, cx| {
            planning.move_to(&names[0], Section::Draft, cx);
            planning.draft_with_agent(&names[..1], cx);
        });
        cx.run_until_parked();
        planning.update(cx, |planning, cx| {
            let titles: Vec<&str> = planning
                .stories(Section::Backlog)
                .iter()
                .map(|story| story.title.as_str())
                .collect();
            assert!(titles.contains(&"Grounded") && !titles.contains(&"Ungrounded"));
            let waiting = planning.waiting(&names[0]);
            assert_eq!(waiting.len(), 1);
            assert_eq!(waiting[0].title, "Ungrounded");
            assert_eq!(planning.story(&names[0]).unwrap().drafted, 1);
            planning.hooks.draft = failing;
            planning.move_to(&names[1], Section::Draft, cx);
            planning.draft_with_agent(&names[1..], cx);
        });
        cx.run_until_parked();
        planning.update(cx, |planning, _| {
            assert!(planning.story_drafts[&names[1]].error.is_some());
            assert_eq!(planning.story(&names[1]).unwrap().drafted, 0);
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A draft splitting one story, in the form drafts gave before they
    /// proposed only new stories: a new story, a rewrite, and a step for
    /// the story drafted.
    fn split_story(
        _: &Path,
        request: &str,
        _: &Cancel,
        _: &dyn Fn(String),
    ) -> anyhow::Result<String> {
        use crate::baked_prompts::planning;
        assert!(request.contains(planning::SPLIT), "it isn't asked to split the story");
        assert!(request.contains(planning::SPLIT_EXAMPLE), "it isn't shown a split");
        let about = request
            .lines()
            .find_map(|line| line.strip_prefix(planning::ABOUT)?.strip_prefix(": "))
            .expect("it isn't told the story")
            .trim()
            .to_string();
        Ok(format!(
            "```suspense-story\ntitle: Apart\nmode: chain\ntarget: new\nspec:\n- No match — apart\n\nThe other piece.\n```\n\n```suspense-story\ntitle: Whole\nmode: code\ntarget: rewrite\nspec:\n- No match — whole\n\nThe first piece.\n```\n\n```suspense-story\ntitle: Then\nmode: code\ntarget: {about}\nspec:\n- No match — after\n\nThen the first.\n```\n"
        ))
    }

    /// A draft holding more than one piece splits its story into new
    /// stories, in the order given, the step part of the one before it;
    /// accepting one adds it at the Backlog's end, "From agent", and
    /// "Accept all" the rest, the story drafted left as it was.
    #[gpui_kit::test]
    async fn a_draft_splits_its_story_into_new_stories(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-draft-split-test", &["Rough", "Other"]);
        let (planning, _) = open_planning(dir.clone(), split_story, cx);
        planning.update(cx, |planning, cx| {
            planning.draft_with_agent(&names[..1], cx);
        });
        cx.run_until_parked();
        planning.update(cx, |planning, cx| {
            let ids = planning.proposal_ids(Some(&names[0]));
            assert_eq!(ids.len(), 2);
            let waiting = planning.waiting(&names[0]);
            assert_eq!(waiting[0].title, "Apart");
            assert_eq!(waiting[1].title, "Whole");
            assert!(waiting.iter().all(|p| p.target.is_none() && !p.rewrite));
            assert_eq!(waiting[1].step.as_ref().unwrap().prompt, "Then the first.");
            let added = planning.accept(&ids[0], cx).unwrap();
            let backlog = planning.stories(Section::Backlog);
            assert_eq!(backlog.last().unwrap().name, added);
            assert!(planning.story(&added).unwrap().from_agent);
            assert_eq!(planning.find(&names[0]).unwrap().0, Section::Backlog);
            assert_eq!(planning.waiting(&names[0]).len(), 1);
            planning.accept_all(Some(&names[0]), cx);
            assert!(planning.waiting(&names[0]).is_empty());
            let whole = planning.stories(Section::Backlog).last().unwrap();
            assert_eq!(whole.title, "Whole");
            assert_eq!(whole.steps().len(), 1);
            let story = planning.story(&names[0]).unwrap();
            assert_eq!(story.title, "Rough");
            assert!(story.steps().is_empty() && !story.updated);
            assert_eq!(story.drafted, 0);
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A story in Draft has its split created straight away, in the order
    /// given, at the Backlog's end; drafted again, the new stories go after
    /// the ones before.
    #[gpui_kit::test]
    async fn a_draft_story_splits_into_created_stories(cx: &mut TestAppContext) {
        let (dir, names) = backlog("planning-draft-created-test", &["Rough"]);
        let (planning, _) = open_planning(dir.clone(), split_story, cx);
        let titles = |planning: &Planning| -> Vec<String> {
            planning
                .stories(Section::Backlog)
                .iter()
                .map(|story| story.title.clone())
                .collect()
        };
        planning.update(cx, |planning, cx| {
            planning.move_to(&names[0], Section::Draft, cx);
            planning.draft_with_agent(&names, cx);
        });
        cx.run_until_parked();
        planning.update(cx, |planning, cx| {
            assert_eq!(titles(planning), ["Apart", "Whole"]);
            let whole = planning.stories(Section::Backlog).last().unwrap();
            assert_eq!(whole.steps().len(), 1);
            assert!(whole.from_agent);
            assert!(planning.waiting(&names[0]).is_empty());
            let story = planning.story(&names[0]).unwrap();
            assert_eq!((story.title.as_str(), story.drafted), ("Rough", 2));
            assert_eq!(planning.find(&names[0]).unwrap().0, Section::Draft);
            planning.draft_with_agent(&names, cx);
        });
        cx.run_until_parked();
        planning.update(cx, |planning, _| {
            assert_eq!(titles(planning), ["Apart", "Whole", "Apart", "Whole"]);
            assert_eq!(planning.story(&names[0]).unwrap().drafted, 4);
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Planning in a window `width` wide, its Backlog holding a story, for
    /// the board's scrollbar.
    fn board_bar_planning(
        name: &str,
        width: f32,
        cx: &mut TestAppContext,
    ) -> (Entity<Planning>, AnyWindowHandle, PathBuf, Vec<String>) {
        use gpui_kit::{px, size};
        let (dir, names) = backlog(name, &["Queue"]);
        let (planning, handle) = open_planning(dir.clone(), unused, cx);
        cx.simulate_window_resize(handle, size(px(width), px(600.)));
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap();
        (planning, handle, dir, names)
    }

    /// How far the board is scrolled sideways, and how far it can scroll.
    fn board_scrolled(
        planning: &Entity<Planning>,
        cx: &mut TestAppContext,
    ) -> (gpui_kit::Pixels, gpui_kit::Pixels) {
        planning.read_with(cx, |planning, _| {
            (
                planning.board_scroll.offset().x,
                planning.board_scroll.max_offset().x,
            )
        })
    }

    /// The board's scrollbar as last drawn: its track, and the thumb in it,
    /// as its left and width, found among what was painted: the raised
    /// rectangle the track's full height, with its single column at each
    /// end.
    fn board_thumb(
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::App,
    ) -> (gpui_kit::Bounds<gpui_kit::Pixels>, f32, f32) {
        use gpui_kit::component::ActiveTheme as _;
        window.render_frame(cx);
        let scale = window.scale_factor();
        let track = window.find("planning-board-scroll-track").bounds();
        let raised = crate::scrollbar::scroll_colors(cx.theme().is_dark()).raised;
        let (left, right) = (track.left().as_f32(), track.right().as_f32());
        let body = window
            .painted_quads()
            .into_iter()
            .map(|quad| (quad.background.as_solid(), quad.bounds))
            .find(|(color, b)| {
                *color == Some(raised)
                    && (b.size.height.0 / scale - track.size.height.as_f32()).abs() < 0.01
                    && (b.origin.y.0 / scale - track.top().as_f32()).abs() < 0.01
                    && b.origin.x.0 / scale >= left - 0.01
                    && (b.origin.x.0 + b.size.width.0) / scale <= right + 0.01
            })
            .map(|(_, b)| b)
            .expect("no thumb in the board's scrollbar");
        (
            track,
            body.origin.x.0 / scale - 1.,
            body.size.width.0 / scale + 2.,
        )
    }

    /// The thumb is drawn where the board's own scroll puts it: as wide as
    /// the share of the board in view, never too narrow to grab, and as far
    /// along as the board is scrolled.
    fn assert_thumb_follows_the_board(
        planning: &Entity<Planning>,
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::App,
        when: &str,
    ) {
        let (track, left, width) = board_thumb(window, cx);
        let handle: crate::scrollbar::Scroll = (&planning.read(cx).board_scroll).into();
        let (at, wide) = crate::scrollbar::sideways_thumb_for_test(&handle, track);
        assert!(
            (left - at.as_f32()).abs() < 0.51 && (width - wide.as_f32()).abs() < 0.51,
            "{when}: the thumb is drawn at {left} {width} wide, not {at:?} {wide:?} wide"
        );
        // And worked out afresh, from the board alone.
        let board = &planning.read(cx).board_scroll;
        let (view, max) = (board.bounds().size.width.as_f32(), board.max_offset().x.as_f32());
        let length = track.size.width.as_f32();
        let expected = (length * view / (view + max)).max(16.).min(length);
        assert!((width - expected).abs() < 0.51, "{when}: {width} wide, not {expected}");
        let scrolled = (-board.offset().x.as_f32()).clamp(0., max);
        let along = if max > 0. { (length - expected) * scrolled / max } else { 0. };
        let expected = track.left().as_f32() + along;
        assert!((left - expected).abs() < 0.51, "{when}: at {left}, not {expected}");
    }

    /// While the columns are wider than the tab, the bar's right button
    /// scrolls the board right a little, and its left button back left.
    #[gpui_kit::test]
    async fn the_board_bar_buttons_scroll_it_a_little(cx: &mut TestAppContext) {
        use gpui_kit::px;
        let (planning, handle, dir, _) =
            board_bar_planning("planning-bar-buttons-test", 900., cx);
        let (at, max) = board_scrolled(&planning, cx);
        assert!(max > crate::scrollbar::STEP, "the board has too little to scroll");
        assert_eq!(at, px(0.));
        let click = |id: &'static str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.click(SharedString::from(id), cx);
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        click("planning-board-scroll-down", cx);
        assert_eq!(board_scrolled(&planning, cx).0, -crate::scrollbar::STEP);
        click("planning-board-scroll-down", cx);
        assert_eq!(board_scrolled(&planning, cx).0, -crate::scrollbar::STEP * 2.);
        click("planning-board-scroll-up", cx);
        assert_eq!(board_scrolled(&planning, cx).0, -crate::scrollbar::STEP);
        click("planning-board-scroll-up", cx);
        click("planning-board-scroll-up", cx);
        assert_eq!(board_scrolled(&planning, cx).0, px(0.), "scrolled past the left");
        cx.update_window(handle, |_, window, cx| {
            assert_thumb_follows_the_board(&planning, window, cx, "after the buttons")
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Pressing the thumb and dragging scrolls the board with it, the thumb
    /// staying under the pointer the whole way, even once the pointer leaves
    /// the bar, until the button is let go.
    #[gpui_kit::test]
    async fn dragging_the_board_thumb_scrolls_the_board(cx: &mut TestAppContext) {
        use gpui_kit::{Modifiers, MouseButton, VisualTestContext, point, px};
        let (planning, handle, dir, _) = board_bar_planning("planning-bar-drag-test", 900., cx);
        let (track, left, width) = cx
            .update_window(handle, |_, window, cx| board_thumb(window, cx))
            .unwrap();
        let grab = point(px(left + 10.), track.center().y);
        let cx = &mut VisualTestContext::from_window(handle, cx);
        cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(board_scrolled(&planning, cx).0, px(0.), "pressing it scrolled");
        // Along the bar, then well above it, over the board.
        let mut last = px(0.);
        let mid = track.center().y.as_f32();
        for (dx, y) in [(40., mid), (120., mid), (200., 150.)] {
            let to = point(grab.x + px(dx), px(y));
            cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
            cx.run_until_parked();
            let (at, _) = board_scrolled(&planning, cx);
            assert!(at < last, "dragging {dx} right didn't scroll the board on");
            last = at;
            cx.update(|window, cx| {
                let (_, now, _) = board_thumb(window, cx);
                assert!(
                    (now - (left + dx)).abs() < 0.51,
                    "dragged {dx}, the thumb is at {now}, not under the pointer"
                );
                assert_thumb_follows_the_board(&planning, window, cx, "dragging");
            });
        }
        // Past the right end, the board is scrolled all the way.
        let far = point(grab.x + px(2000.), px(150.));
        cx.simulate_mouse_move(far, Some(MouseButton::Left), Modifiers::default());
        cx.run_until_parked();
        let (at, max) = board_scrolled(&planning, cx);
        assert_eq!(at, -max);
        cx.update(|window, cx| {
            let (_, now, _) = board_thumb(window, cx);
            assert!((now + width - track.right().as_f32()).abs() < 0.51, "{now}");
        });
        // Let go, moving no longer scrolls.
        cx.simulate_mouse_up(far, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(grab, None, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(board_scrolled(&planning, cx).0, -max, "it scrolled once let go");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Pressing the track off the thumb centres the thumb on the press, and
    /// it can be dragged on from there straight away.
    #[gpui_kit::test]
    async fn pressing_the_board_track_moves_the_thumb_there(cx: &mut TestAppContext) {
        use gpui_kit::{Modifiers, MouseButton, VisualTestContext, point, px};
        let (planning, handle, dir, _) = board_bar_planning("planning-bar-track-test", 900., cx);
        let (track, left, width) = cx
            .update_window(handle, |_, window, cx| board_thumb(window, cx))
            .unwrap();
        assert!((left - track.left().as_f32()).abs() < 0.51, "the thumb starts at {left}");
        // Off the thumb, far enough in that it isn't held at the track's end.
        let press = point(px(left + width + 60.), track.center().y);
        assert!(press.x.as_f32() + width / 2. < track.right().as_f32());
        let cx = &mut VisualTestContext::from_window(handle, cx);
        cx.simulate_mouse_down(press, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert!(board_scrolled(&planning, cx).0 < px(0.), "pressing the track didn't scroll");
        cx.update(|window, cx| {
            let (_, now, _) = board_thumb(window, cx);
            assert!(
                (now + width / 2. - press.x.as_f32()).abs() < 0.51,
                "the thumb at {now} isn't centred on the press"
            );
            assert_thumb_follows_the_board(&planning, window, cx, "pressed");
        });
        // Dragged on, without letting go.
        let on = point(press.x + px(30.), press.y);
        cx.simulate_mouse_move(on, Some(MouseButton::Left), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            let (_, now, _) = board_thumb(window, cx);
            assert!(
                (now + width / 2. - on.x.as_f32()).abs() < 0.51,
                "the thumb at {now} didn't follow the drag"
            );
        });
        cx.simulate_mouse_up(on, MouseButton::Left, Modifiers::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Where a resize edge's hit area overlaps the bar, a press goes to the
    /// smaller of the two: the edge takes it over the track, and a button
    /// takes it over the edge.
    #[gpui_kit::test]
    async fn the_smaller_of_the_board_bar_and_an_edge_takes_a_press(cx: &mut TestAppContext) {
        use gpui_kit::{Bounds, Modifiers, MouseButton, VisualTestContext, point, px, size};
        let (planning, handle, dir, _) = board_bar_planning("planning-bar-edge-test", 900., cx);
        let (track, left, width, right) = cx
            .update_window(handle, |_, window, cx| {
                let (track, left, width) = board_thumb(window, cx);
                let right = window.find("planning-board-scroll-down").bounds();
                (track, left, width, right)
            })
            .unwrap();
        // An edge's strip, 8 pixels wide and the window's height, standing
        // over the track off the thumb, and another over the right button.
        let off_thumb = px(left + width + 60.);
        let edge = |x: gpui_kit::Pixels| {
            Bounds::new(point(x - px(4.), px(0.)), size(px(8.), px(600.)))
        };
        cx.update(|cx| {
            crate::hit_areas::register_resize("over-track".into(), edge(off_thumb), cx);
            crate::hit_areas::register_resize("over-button".into(), edge(right.center().x), cx);
        });
        let cx = &mut VisualTestContext::from_window(handle, cx);
        let at = point(off_thumb, track.center().y);
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(board_scrolled(&planning, cx).0, px(0.), "the track took the edge's press");
        cx.simulate_mouse_down(right.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(right.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            board_scrolled(&planning, cx).0,
            -crate::scrollbar::STEP,
            "the button didn't take the press over the edge"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// While the columns fit, the thumb fills the track and nothing in the
    /// bar scrolls the board.
    #[gpui_kit::test]
    async fn the_board_bar_scrolls_nothing_while_the_columns_fit(cx: &mut TestAppContext) {
        use gpui_kit::{Modifiers, MouseButton, VisualTestContext, point, px};
        let (planning, handle, dir, _) = board_bar_planning("planning-bar-fit-test", 2400., cx);
        assert_eq!(board_scrolled(&planning, cx), (px(0.), px(0.)), "the board scrolls");
        let (track, left, width) = cx
            .update_window(handle, |_, window, cx| {
                window.click(SharedString::from("planning-board-scroll-down"), cx);
                window.click(SharedString::from("planning-board-scroll-up"), cx);
                board_thumb(window, cx)
            })
            .unwrap();
        assert!(
            (left - track.left().as_f32()).abs() < 0.51
                && (width - track.size.width.as_f32()).abs() < 0.51,
            "the thumb is {width} wide at {left}, not the whole track"
        );
        let cx = &mut VisualTestContext::from_window(handle, cx);
        let at = point(track.center().x, track.center().y);
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
        let on = at + point(px(200.), px(0.));
        cx.simulate_mouse_move(on, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(board_scrolled(&planning, cx).0, px(0.), "the board scrolled");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Opening and closing the side panel narrows and widens the board,
    /// and on every frame of the slide the thumb's size and place follow
    /// the board's sideways scroll exactly.
    #[gpui_kit::test]
    async fn the_board_thumb_follows_the_side_panel(cx: &mut TestAppContext) {
        let (planning, handle, dir, names) =
            board_bar_planning("planning-bar-panel-test", 1400., cx);
        // Scrolled all the way, so widening the board scrolls it back.
        cx.update_window(handle, |_, window, cx| {
            let board = planning.read(cx).board_scroll.clone();
            board.set_offset(gpui_kit::point(-board.max_offset().x, gpui_kit::px(0.)));
            assert_thumb_follows_the_board(&planning, window, cx, "scrolled to the end");
        })
        .unwrap();
        let wide = board_scrolled(&planning, cx).1;
        crate::animations::slide::AT_ONCE.set(false);
        for open in [true, false] {
            cx.update_window(handle, |_, window, cx| {
                planning.update(cx, |planning, cx| {
                    if open {
                        planning.open_story(&names[0], window, cx)
                    } else {
                        planning.close_story(window, cx)
                    }
                });
                let start = std::time::Instant::now();
                while start.elapsed() < crate::animations::slide::QUICK * 2 {
                    let doing = if open { "opening" } else { "closing" };
                    let when = format!("{doing} at {:?}", start.elapsed());
                    assert_thumb_follows_the_board(&planning, window, cx, &when);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            })
            .unwrap();
            cx.run_until_parked();
            let (_, max) = board_scrolled(&planning, cx);
            if open {
                assert!(max > wide, "the side panel didn't narrow the board");
            } else {
                assert_eq!(max, wide, "closing the side panel didn't widen the board");
            }
        }
        crate::animations::slide::AT_ONCE.set(true);
        std::fs::remove_dir_all(&dir).ok();
    }
}
