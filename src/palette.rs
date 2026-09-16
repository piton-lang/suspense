//! The palette: a popover, opened with Ctrl/Cmd+P, for fuzzy searching the
//! project's files, the application's own commands, and the harness's
//! commands, skills, and agents, each under its own tab. Picking a result
//! closes the palette and hands what was picked to the main window.
//!
//! On the Files tab the search can also be handed to the harness, which looks
//! through the project for the file described: Ctrl/Cmd+Enter at any time, or
//! Enter when nothing matches. The run's output table takes the place of the
//! results while it looks.

use std::borrow::Cow;
use std::ops::Range;
use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::assets::IconName;
use gpui_kit::base::actions::Confirm;
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::input::{Enter, IndentInline, OutdentInline};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::Quit;
use crate::fuzzy;
use crate::harness::{self, HarnessEvent};
use crate::harness_mentions::{self, Invocable, MentionKind};
use crate::hidden_anchor;
use crate::project_directory::ProjectDirectory;
use crate::prompt_mode::{Reply, output_table};

actions!(palette, [SearchFiles]);

const CONTEXT: &str = "Palette";

/// Ctrl/Cmd+Enter hands the search to the harness. The search field's own
/// Ctrl/Cmd+Enter is caught in the palette too.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "secondary-enter",
        SearchFiles,
        Some(CONTEXT),
    )]);
}

/// Appended to the harness's system prompt for a file search in `project_dir`.
/// The spec and code locations are read from its `piton.config.pi`; any it
/// does not set are left out of the search order.
fn search_system_prompt(project_dir: &Path) -> String {
    let spec = hidden_anchor::config_value(project_dir, "root").ok();
    let code = hidden_anchor::config_value(project_dir, "codeRoot").ok();
    let order = [
        spec.map(|spec| format!("the spec, in {spec}")),
        code.map(|code| format!("the code, in {code}")),
        Some(hidden_anchor::APP_DIR.to_string()),
        Some(".claude".to_string()),
    ]
    .into_iter()
    .flatten()
    .enumerate()
    .map(|(index, place)| format!("{}. {place}", index + 1))
    .collect::<Vec<_>>()
    .join("\n");

    format!(
        "\
You are finding a file in this project for someone who could not find it by \
its name. They describe what they are looking for; it may be a partial or \
misremembered name, what the file does, or something it contains.

Your goal is to identify the one file they mean. Search with tools that only \
read, such as Glob, Grep and Read, and never change anything. Stop searching as \
soon as you are confident. Only when several files are all genuinely and \
equally what was asked for, give each of them; otherwise give the single best \
file.

Look in these places in this order of priority, preferring a match from an \
earlier one over an equally good match from a later one:
{order}

End with a reply of nothing but JSON, with no prose and no code fence, listing \
paths relative to the project directory: {{\"files\": [\"path/to/file\"]}}. If \
nothing matches, reply {{\"files\": []}}."
    )
}

/// Runs the harness: [`harness::send`], replaced in tests.
type SendToHarness = fn(
    String,
    Option<String>,
    Option<harness::Resume>,
    PathBuf,
) -> mpsc::UnboundedReceiver<HarnessEvent>;

/// A search handed to the harness, and what the harness did with it.
struct FileSearch {
    query: SharedString,
    reply: Reply,
    scroll: ScrollHandle,
    _task: Task<()>,
}

/// The files the harness's final reply names that exist in the project, in
/// the order given. The reply's JSON is looked for between its first `{` and
/// last `}`, in case it strays from JSON alone.
fn found_files(reply: &str, root: &Path) -> Vec<PathBuf> {
    let json = match (reply.find('{'), reply.rfind('}')) {
        (Some(start), Some(end)) if start < end => &reply[start..=end],
        _ => return Vec::new(),
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for path in value
        .get("files")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
    {
        let path = root.join(path.trim_start_matches("./"));
        if path.is_file() && !files.contains(&path) {
            files.push(path);
        }
    }
    files
}

/// The most results a tab lists.
const MAX_RESULTS: usize = 100;

/// The most files read from a project.
const MAX_FILES: usize = 100_000;

const WIDTH: Pixels = px(640.);
const MAX_LIST_HEIGHT: Pixels = px(420.);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaletteTab {
    Files,
    System,
    Commands,
    Skills,
    Agents,
}

impl PaletteTab {
    /// In the order they are shown, and Tab cycles them.
    const ALL: [Self; 5] = [
        Self::Files,
        Self::System,
        Self::Commands,
        Self::Skills,
        Self::Agents,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::System => "System",
            Self::Commands => "Commands",
            Self::Skills => "Skills",
            Self::Agents => "Agents",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Files => "Search files…",
            Self::System => "Search the application's commands…",
            Self::Commands => "Search the harness's commands…",
            Self::Skills => "Search skills…",
            Self::Agents => "Search agents…",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Files => IconName::File,
            Self::System => IconName::Wrench,
            Self::Commands => IconName::SquareTerminal,
            Self::Skills => IconName::BookOpen,
            Self::Agents => IconName::Bot,
        }
    }

    fn mention_kind(self) -> Option<MentionKind> {
        match self {
            Self::Files | Self::System => None,
            Self::Commands => Some(MentionKind::Command),
            Self::Skills => Some(MentionKind::Skill),
            Self::Agents => Some(MentionKind::Agent),
        }
    }
}

/// The application's own commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemCommand {
    OpenProject,
    Build,
    ToggleDarkMode,
    Settings,
    Quit,
}

/// What the application's commands depend on, as of opening the palette.
#[derive(Clone, Copy, Debug)]
pub struct SystemState {
    pub can_build: bool,
    pub dark: bool,
}

impl SystemState {
    fn commands(self) -> Vec<Candidate> {
        let theme = if self.dark { "Light mode" } else { "Dark mode" };
        [
            (SystemCommand::OpenProject, "Open Project…", true),
            (SystemCommand::Build, "Build Spec", self.can_build),
            (SystemCommand::ToggleDarkMode, theme, true),
            (SystemCommand::Settings, "Settings…", true),
            (SystemCommand::Quit, "Quit", true),
        ]
        .into_iter()
        .map(|(command, label, enabled)| Candidate::System {
            command,
            label: label.into(),
            enabled,
        })
        .collect()
    }
}

/// Emitted when a result is picked.
#[derive(Debug, PartialEq)]
pub enum Picked {
    File(PathBuf),
    System(SystemCommand),
    /// A harness mention to insert, as typed: `/name ` or `@agent-name `.
    Mention(String),
}

#[derive(Clone, Debug)]
enum Candidate {
    File {
        path: PathBuf,
        /// The path in the project, with `/` between folders.
        label: SharedString,
        /// Byte offset of the file's name in `label`.
        name_start: usize,
    },
    System {
        command: SystemCommand,
        label: SharedString,
        enabled: bool,
    },
    Mention(Invocable),
}

impl Candidate {
    fn text(&self) -> &str {
        match self {
            Self::File { label, .. } | Self::System { label, .. } => label,
            Self::Mention(invocable) => &invocable.name,
        }
    }

    fn name_start(&self) -> usize {
        match self {
            Self::File { name_start, .. } => *name_start,
            _ => 0,
        }
    }
}

/// A result as listed: the candidate and the byte offsets of its characters
/// the search matched.
struct Shown {
    candidate: Candidate,
    positions: Vec<usize>,
}

/// The candidates the query matches, best first, up to [`MAX_RESULTS`]. An
/// empty query keeps them in their order.
fn rank(query: &str, candidates: &[Candidate]) -> Vec<Shown> {
    let mut matched: Vec<(i32, &Candidate, Vec<usize>)> = candidates
        .iter()
        .filter_map(|candidate| {
            let found = fuzzy::fuzzy_match(query, candidate.text(), candidate.name_start())?;
            Some((found.score, candidate, found.positions))
        })
        .collect();
    if !query.trim().is_empty() {
        matched.sort_by(|(a_score, a, _), (b_score, b, _)| {
            b_score
                .cmp(a_score)
                .then_with(|| a.text().len().cmp(&b.text().len()))
                .then_with(|| a.text().cmp(b.text()))
        });
    }
    matched
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, candidate, positions)| Shown {
            candidate: candidate.clone(),
            positions,
        })
        .collect()
}

/// Every file under `root`, by its path in the project, leaving out `.git`
/// and whatever the project's `.gitignore` files ignore. Blocking.
fn list_files(root: &Path) -> Vec<Candidate> {
    let mut files: Vec<Candidate> = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .take(MAX_FILES)
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(root).ok()?;
            let label = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let name_start = label.rfind('/').map_or(0, |ix| ix + 1);
            Some(Candidate::File {
                path: entry.path().to_path_buf(),
                label: label.into(),
                name_start,
            })
        })
        .collect();
    files.sort_by_cached_key(|file| file.text().to_lowercase());
    files
}

pub struct Palette {
    /// Tracks the palette's elements, so focus inside them means it is open.
    focus_handle: FocusHandle,
    /// Replaced when the tab changes, so the highlight starts on the first
    /// result rather than on the row it was on.
    command: Entity<CommandState>,
    tab: PaletteTab,
    query: SharedString,
    system: SystemState,
    project: Option<PathBuf>,
    /// `None` while they are read.
    files: Option<Vec<Candidate>>,
    invocables: Option<Vec<Invocable>>,
    shown: Vec<Shown>,
    /// The Files tab's search as handed to the harness, shown in place of
    /// the results.
    search: Option<FileSearch>,
    send_to_harness: SendToHarness,
    _loads: Vec<Task<()>>,
}

impl EventEmitter<Picked> for Palette {}

impl Palette {
    /// A palette on the Files tab, reading the project's files and the
    /// harness's commands, skills, and agents in the background.
    pub fn new(system: SystemState, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = ProjectDirectory::get(cx);

        let mut loads = Vec::new();
        if let Some(root) = project.clone() {
            let read = cx.background_spawn(async move { list_files(&root) });
            loads.push(cx.spawn(async move |this, cx| {
                let files = read.await;
                this.update(cx, |this, cx| {
                    this.files = Some(files);
                    this.refresh(cx);
                })
                .ok();
            }));
        }
        let discover = cx.background_spawn({
            let project = project.clone();
            async move {
                let mut invocables = harness_mentions::discover(project.as_deref());
                invocables.sort_by_cached_key(|invocable| invocable.name.to_lowercase());
                invocables
            }
        });
        loads.push(cx.spawn(async move |this, cx| {
            let invocables = discover.await;
            this.update(cx, |this, cx| {
                this.invocables = Some(invocables);
                this.refresh(cx);
            })
            .ok();
        }));

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            command: cx.new(|cx| CommandState::new(window, cx)),
            tab: PaletteTab::Files,
            query: SharedString::default(),
            system,
            project,
            files: None,
            invocables: None,
            shown: Vec::new(),
            search: None,
            send_to_harness: harness::send,
            _loads: loads,
        };
        this.refresh(cx);
        this
    }

    /// Shows `palette` in a dialog over the window, with focus in its search.
    pub fn open(palette: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let content = palette.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let content = content.clone();
            dialog
                .w(WIDTH)
                .p_0()
                .close_button(false)
                .content(move |body, _, _| body.child(content.clone()))
        });
        let command = palette.read(cx).command.clone();
        command.update(cx, |command, cx| command.focus(window, cx));
    }

    /// Whether the palette is open: focus is somewhere inside it.
    pub fn is_open(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
    }

    /// What Esc does: clears the search, or closes the palette once the search
    /// is empty.
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command.read(cx).query(cx).is_empty() {
            window.close_dialog(cx);
        } else {
            self.command
                .update(cx, |command, cx| command.set_query("", window, cx));
        }
    }

    #[cfg(test)]
    pub fn result_labels(&self) -> Vec<String> {
        self.shown
            .iter()
            .map(|shown| shown.candidate.text().to_string())
            .collect()
    }

    fn candidates(&self) -> Cow<'_, [Candidate]> {
        match self.tab.mention_kind() {
            None if self.tab == PaletteTab::Files => {
                Cow::Borrowed(self.files.as_deref().unwrap_or_default())
            }
            None => Cow::Owned(self.system.commands()),
            Some(kind) => Cow::Owned(
                self.invocables
                    .iter()
                    .flatten()
                    .filter(|invocable| invocable.kind == kind)
                    .cloned()
                    .map(Candidate::Mention)
                    .collect(),
            ),
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let shown = rank(&self.query, &self.candidates());
        self.shown = shown;
        cx.notify();
    }

    #[cfg(test)]
    pub fn set_send_to_harness(&mut self, send: SendToHarness) {
        self.send_to_harness = send;
    }

    /// Whether the harness's search is showing in place of the results.
    #[cfg(test)]
    pub fn is_searching_files(&self) -> bool {
        self.search.is_some()
    }

    fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if query != self.query.as_ref() {
            self.stop_search(cx);
        }
        self.query = query.to_string().into();
        self.refresh(cx);
    }

    /// Whether the search can be handed to the harness.
    fn can_search_files(&self) -> bool {
        self.tab == PaletteTab::Files && self.project.is_some() && !self.query.trim().is_empty()
    }

    /// Hands the search to the harness to find the file it describes. One
    /// file found is picked; several are only reported for now. The run's
    /// output shows in place of the results meanwhile.
    fn search_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.project.clone().filter(|_| self.can_search_files()) else {
            return;
        };
        if self
            .search
            .as_ref()
            .is_some_and(|search| search.query == self.query && !search.reply.is_done())
        {
            return;
        }
        self.stop_search(cx);

        let query = self.query.clone();
        let prompt = format!("Find the file I'm looking for: {query}");
        let mut events = (self.send_to_harness)(
            prompt,
            Some(search_system_prompt(&root)),
            None,
            root.clone(),
        );
        let task = cx.spawn_in(window, async move |this, cx| {
            let mut answer = None;
            while let Some(event) = events.next().await {
                if let HarnessEvent::Finished {
                    is_error: false,
                    result,
                } = &event
                {
                    answer = Some(result.clone());
                }
                let applied = this.update(cx, |this, cx| {
                    let Some(search) = &mut this.search else {
                        return;
                    };
                    if let Some(error) = search.reply.apply(event) {
                        search.reply.push_error(error);
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
            }

            let answered = answer.is_some();
            let files = match answer {
                Some(answer) => {
                    let root = root.clone();
                    cx.background_spawn(async move { found_files(&answer, &root) })
                        .await
                }
                None => Vec::new(),
            };
            this.update_in(cx, |this, window, cx| {
                let Some(search) = &mut this.search else {
                    return;
                };
                if !search.reply.is_done()
                    && let Some(error) = search.reply.apply(HarnessEvent::Failed(
                        "The harness stopped without a result.".into(),
                    ))
                {
                    search.reply.push_error(error);
                }
                // A failed run already shows why.
                if answered && files.is_empty() {
                    search
                        .reply
                        .push_error("No file matching the search was found.".into());
                }
                cx.notify();
                // Left alone once the palette has closed.
                if !this.is_open(window, cx) {
                    return;
                }
                match files.as_slice() {
                    [] => {}
                    [file] => {
                        window.close_dialog(cx);
                        cx.emit(Picked::File(file.clone()));
                    }
                    _ => window.open_alert_dialog(cx, |alert, _, _| {
                        alert.title("Multiple results were found")
                    }),
                }
            })
            .ok();
        });
        self.search = Some(FileSearch {
            query,
            reply: Reply::default(),
            scroll: ScrollHandle::new(),
            _task: task,
        });
        cx.notify();
    }

    /// Drops the harness's search, stopping its run if still going.
    fn stop_search(&mut self, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            cx.notify();
        }
    }

    fn set_tab(&mut self, tab: PaletteTab, window: &mut Window, cx: &mut Context<Self>) {
        if tab == self.tab {
            return;
        }
        self.stop_search(cx);
        self.tab = tab;
        let query = self.query.clone();
        self.command = cx.new(|cx| {
            let mut command = CommandState::new(window, cx);
            command.set_query(query, window, cx);
            command
        });
        self.command
            .update(cx, |command, cx| command.focus(window, cx));
        self.refresh(cx);
    }

    /// Moves `step` tabs along, wrapping around.
    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = PaletteTab::ALL.len() as isize;
        let ix = PaletteTab::ALL
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0) as isize;
        let next = PaletteTab::ALL[(ix + step).rem_euclid(count) as usize];
        self.set_tab(next, window, cx);
    }

    fn pick(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = self.shown.get(row) else {
            return;
        };
        let picked = match &shown.candidate {
            Candidate::File { path, .. } => Picked::File(path.clone()),
            Candidate::System { enabled: false, .. } => return,
            Candidate::System { command, .. } => Picked::System(*command),
            Candidate::Mention(invocable) => Picked::Mention(invocable.mention()),
        };
        window.close_dialog(cx);
        cx.emit(picked);
    }

    fn empty_text(&self) -> &'static str {
        let loading = match self.tab {
            PaletteTab::Files if self.project.is_none() => return "No project open",
            PaletteTab::Files => self.files.is_none(),
            PaletteTab::System => false,
            _ => self.invocables.is_none(),
        };
        if loading {
            "Loading…"
        } else if !self.query.trim().is_empty() {
            "No matches"
        } else {
            match self.tab {
                PaletteTab::Files => "No files",
                PaletteTab::Commands => "No commands",
                PaletteTab::Skills => "No skills",
                PaletteTab::Agents => "No agents",
                PaletteTab::System => "",
            }
        }
    }
}

/// A result's row: its icon, its text with the matched characters
/// highlighted, and a description or shortcut when it has one.
fn item(shown: &Shown, icon: IconName, cx: &App) -> CommandItem {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let highlight = HighlightStyle {
        color: Some(theme.primary),
        font_weight: Some(FontWeight::BOLD),
        ..HighlightStyle::default()
    };
    let text: SharedString = shown.candidate.text().to_string().into();
    let highlights: Vec<(Range<usize>, HighlightStyle)> = shown
        .positions
        .iter()
        .map(|&start| {
            let len = text[start..].chars().next().map_or(1, char::len_utf8);
            (start..start + len, highlight)
        })
        .collect();
    let description = match &shown.candidate {
        Candidate::Mention(invocable) => invocable.description.clone(),
        _ => None,
    };
    let enabled = !matches!(shown.candidate, Candidate::System { enabled: false, .. });
    let quit = matches!(
        shown.candidate,
        Candidate::System {
            command: SystemCommand::Quit,
            ..
        }
    );

    CommandItem::new()
        .label(text.clone())
        .disabled(!enabled)
        .child(move |window, _| {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(
                    Icon::new(icon.clone())
                        .small()
                        .flex_none()
                        .text_color(muted),
                )
                .child(
                    div()
                        .flex_none()
                        .max_w(relative(0.6))
                        .truncate()
                        .child(StyledText::new(text.clone()).with_highlights(highlights.clone())),
                )
                .when_some(description.clone(), |row, description| {
                    row.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(muted)
                            .child(description),
                    )
                })
                .when(quit, |row| {
                    row.children(
                        Kbd::binding_for_action(&Quit, None, window).map(|kbd| kbd.ml_auto()),
                    )
                })
        })
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let icon = self.tab.icon();
        let searching = self.search.is_some();
        // The harness's search takes the results' place.
        let items: Vec<CommandItem> = if searching {
            Vec::new()
        } else {
            self.shown
                .iter()
                .map(|shown| item(shown, icon.clone(), cx))
                .collect()
        };
        let empty: SharedString = self.empty_text().into();
        let selected_tab = PaletteTab::ALL
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0);

        let command = Command::new(&self.command)
            .bordered(false)
            // Results are fuzzy matched and ranked here instead.
            .filterable(false)
            .max_h(MAX_LIST_HEIGHT)
            .placeholder(self.tab.placeholder())
            .header({
                let this = this.clone();
                move |_, _, _| {
                    let this = this.clone();
                    TabBar::new("palette-tabs")
                        .selected_index(selected_tab)
                        .on_click(move |ix, window, cx| {
                            this.update(cx, |this, cx| {
                                this.set_tab(PaletteTab::ALL[*ix], window, cx)
                            })
                            .ok();
                        })
                        .children(PaletteTab::ALL.map(|tab| Tab::new().label(tab.label())))
                }
            })
            .items(items)
            .empty({
                let this = this.clone();
                move |_, _, cx| {
                    let palette = this.upgrade();
                    let search = palette
                        .as_ref()
                        .and_then(|palette| palette.read(cx).search.as_ref());
                    match search {
                        Some(search) => {
                            let table = div()
                                .id("palette-search")
                                .w_full()
                                // Inside the list's padding.
                                .max_h(MAX_LIST_HEIGHT - px(8.))
                                .overflow_y_scroll()
                                .track_scroll(&search.scroll)
                                .child(output_table(0, &search.reply, None, None, cx));
                            // Lets UI tests find the search; inert in normal builds.
                            let table = gpui_kit::TestSupportExt::test_support(table);
                            crate::scroll_column::with_scroll_column(
                                "palette-search",
                                &search.scroll,
                                table,
                                false,
                                None,
                                cx,
                            )
                        }
                        None => div()
                            .py_6()
                            .w_full()
                            .text_center()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(empty.clone())
                            .into_any_element(),
                    }
                }
            })
            .on_query({
                let this = this.clone();
                move |query, _, cx| {
                    this.update(cx, |this, cx| this.set_query(query, cx)).ok();
                }
            })
            .on_confirm(move |ix, window, cx| {
                this.update(cx, |this, cx| this.pick(ix.row, window, cx))
                    .ok();
            });

        let palette = v_flex()
            .id("palette")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .w_full()
            .overflow_hidden()
            // Enter with nothing matched hands the search to the harness.
            .capture_action(cx.listener(|this, action: &Confirm, window, cx| {
                let nothing_found = this.files.is_some() && this.shown.is_empty();
                if !action.secondary && nothing_found && this.can_search_files() {
                    cx.stop_propagation();
                    this.search_files(window, cx);
                }
            }))
            // Ctrl/Cmd+Enter, as the search field reads it.
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if action.secondary && this.tab == PaletteTab::Files {
                    cx.stop_propagation();
                    this.search_files(window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &SearchFiles, window, cx| this.search_files(window, cx)),
            )
            // Tab and Shift+Tab switch tabs rather than reaching the search
            // field, which keeps focus.
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                cx.stop_propagation();
                this.cycle(1, window, cx);
            }))
            .capture_action(cx.listener(|this, _: &OutdentInline, window, cx| {
                cx.stop_propagation();
                this.cycle(-1, window, cx);
            }))
            .child(command);
        // Lets UI tests find the palette; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(palette)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{Candidate, MAX_RESULTS, found_files, list_files, rank, search_system_prompt};

    /// The search looks through this repository's spec and code, from its
    /// `piton.config.pi`, then the application's and the harness's directories.
    #[test]
    fn search_prompt_orders_configured_locations() {
        let prompt = search_system_prompt(Path::new(env!("CARGO_MANIFEST_DIR")));
        assert!(prompt.contains(
            "\n1. the spec, in ./spec\n2. the code, in ./src\n3. .suspense\n4. .claude\n"
        ));
    }

    /// The harness's answer names files that exist, relative to the project,
    /// whether or not it strayed from JSON alone.
    #[test]
    fn reads_the_files_the_harness_found() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/palette-found-test");
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "").unwrap();
        fs::write(root.join("b.md"), "").unwrap();

        assert_eq!(
            found_files(r#"{"files": ["src/a.rs"]}"#, &root),
            [root.join("src/a.rs")]
        );
        assert_eq!(
            found_files(
                "Found them:\n```json\n{\"files\": [\"./b.md\", \"gone.rs\", \"src/a.rs\", \"b.md\"]}\n```",
                &root
            ),
            [root.join("b.md"), root.join("src/a.rs")]
        );
        assert!(found_files(r#"{"files": []}"#, &root).is_empty());
        assert!(found_files("I could not find it.", &root).is_empty());
        fs::remove_dir_all(&root).ok();
    }

    fn file(label: &str) -> Candidate {
        Candidate::File {
            path: PathBuf::from(label),
            label: label.to_string().into(),
            name_start: label.rfind('/').map_or(0, |ix| ix + 1),
        }
    }

    fn texts(query: &str, candidates: &[Candidate]) -> Vec<String> {
        rank(query, candidates)
            .into_iter()
            .map(|shown| shown.candidate.text().to_string())
            .collect()
    }

    #[test]
    fn ranks_the_best_matches_first() {
        let files = [
            file("spec/scope/application/MainWindow.pi"),
            file("src/main_window.rs"),
            file("src/main.rs"),
            file("src/toolbar.rs"),
        ];
        assert_eq!(
            texts("main", &files)[..2],
            ["src/main.rs", "src/main_window.rs"]
        );
        assert_eq!(
            texts("mwin", &files),
            ["src/main_window.rs", "spec/scope/application/MainWindow.pi"]
        );
        assert_eq!(texts("zzz", &files), Vec::<String>::new());
        // An empty search keeps the order.
        assert_eq!(texts("", &files)[0], "spec/scope/application/MainWindow.pi");

        let many: Vec<Candidate> = (0..MAX_RESULTS * 2)
            .map(|ix| file(&format!("f{ix}.rs")))
            .collect();
        assert_eq!(rank("", &many).len(), MAX_RESULTS);
        assert_eq!(rank("f", &many).len(), MAX_RESULTS);
    }

    #[test]
    fn lists_files_by_path_leaving_out_ignored_ones() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/palette-files-test");
        fs::remove_dir_all(&root).ok();
        for path in [
            "src/b.rs",
            "src/A.rs",
            ".claude/skills/x/SKILL.md",
            "build/out.txt",
            ".git/HEAD",
            "README.md",
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        fs::write(root.join(".gitignore"), "/build\n").unwrap();

        let labels: Vec<String> = list_files(&root)
            .iter()
            .map(|file| file.text().to_string())
            .collect();
        assert_eq!(
            labels,
            [
                ".claude/skills/x/SKILL.md",
                ".gitignore",
                "README.md",
                "src/A.rs",
                "src/b.rs"
            ]
        );
        fs::remove_dir_all(&root).ok();
    }
}
