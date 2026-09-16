//! Divergence analysis: how far a project's code and spec have drifted apart.
//! Two read-only agents run at once, one going through the source files and
//! one through the spec files, each saying which files on the other side
//! influenced or implement them, how strongly, and how far each file diverges.
//! Each also says how well defined each file is, and assesses its side as a
//! whole. Their replies are merged into connections between source and spec
//! files, measures of each file, and a divergence score for the whole project,
//! and each report is saved with the project, since analyses are costly.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::hidden_anchor::{self, APP_DIR};

/// The most files of each side analyzed.
pub const MAX_FILES: usize = 400;

/// How strong a connection has to be to count as strong, or as moderate.
pub const STRONG: f32 = 2. / 3.;
pub const MODERATE: f32 = 1. / 3.;

/// The side of the project a file is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Source,
    Spec,
}

impl Side {
    pub fn other(self) -> Self {
        match self {
            Side::Source => Side::Spec,
            Side::Spec => Side::Source,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Side::Source => "Source",
            Side::Spec => "Spec",
        }
    }
}

/// An agent's reply: its assessment of its side, the areas it found well and
/// less well defined, and each file it analyzed, with what it found.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentReply {
    #[serde(default)]
    pub assessment: String,
    #[serde(default)]
    pub well_defined: Vec<Area>,
    #[serde(default)]
    pub less_defined: Vec<Area>,
    pub files: Vec<AgentFile>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct AgentFile {
    pub path: String,
    pub divergence: f32,
    #[serde(default)]
    pub definition: Option<f32>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub gaps: Vec<String>,
    #[serde(default)]
    pub links: Vec<AgentLink>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct AgentLink {
    pub path: String,
    pub strength: f32,
    #[serde(default)]
    pub divergence: Option<f32>,
    #[serde(default)]
    pub note: String,
}

/// An area of the project, and why it is well or less well defined.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Area {
    pub name: String,
    #[serde(default)]
    pub why: String,
}

/// A file on one side, as analyzed.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct FileResult {
    pub path: String,
    /// From 0 to 1; `None` when the agent left it out.
    pub divergence: Option<f32>,
    /// How well defined it is, from 0 to 1; `None` when the agent left it out
    /// or didn't say.
    pub definition: Option<f32>,
    pub summary: String,
    /// The gaps in a spec file, or the blanks a source file filled in.
    pub gaps: Vec<String>,
}

/// A source file and a spec file one influenced or implements the other.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Connection {
    pub source: String,
    pub spec: String,
    /// From 0 to 1: the average of what the agents said.
    pub strength: f32,
    /// How far the two diverge, from 0 to 1: the average of what the agents
    /// said, `None` when neither did.
    pub divergence: Option<f32>,
    pub notes: Vec<String>,
}

impl Connection {
    /// The file at the `side` end of it.
    pub fn end(&self, side: Side) -> &str {
        match side {
            Side::Source => &self.source,
            Side::Spec => &self.spec,
        }
    }
}

/// What the analysis found.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Report {
    pub sources: Vec<FileResult>,
    pub specs: Vec<FileResult>,
    pub connections: Vec<Connection>,
    /// The code agent's assessment of the code, and the spec agent's of the
    /// spec.
    pub code_assessment: String,
    pub spec_assessment: String,
    pub well_defined: Vec<Area>,
    pub less_defined: Vec<Area>,
}

/// A whole percentage of `value`, from 0 to 1.
pub fn percent(value: f32) -> u32 {
    (value.clamp(0., 1.) * 100.).round() as u32
}

/// The average of `values`, as a whole percentage; `None` when there are none.
fn average_percent(values: impl IntoIterator<Item = f32>) -> Option<u32> {
    let values: Vec<f32> = values.into_iter().collect();
    (!values.is_empty()).then(|| percent(values.iter().sum::<f32>() / values.len() as f32))
}

impl Report {
    pub fn files(&self, side: Side) -> &[FileResult] {
        match side {
            Side::Source => &self.sources,
            Side::Spec => &self.specs,
        }
    }

    /// The divergence score, as a whole percentage: the average divergence of
    /// every file analyzed. `None` when nothing was.
    pub fn score(&self) -> Option<u32> {
        average_percent(
            self.sources
                .iter()
                .chain(&self.specs)
                .filter_map(|file| file.divergence),
        )
    }

    /// How aligned the code and spec are, as a whole percentage: 100 less the
    /// divergence score.
    pub fn alignment(&self) -> Option<u32> {
        self.score().map(|score| 100 - score.min(100))
    }

    /// Every file analyzed, source and spec alike, with its side.
    pub fn analyzed(&self) -> impl Iterator<Item = (Side, &FileResult)> {
        let sources = self.sources.iter().map(|file| (Side::Source, file));
        let specs = self.specs.iter().map(|file| (Side::Spec, file));
        sources
            .chain(specs)
            .filter(|(_, file)| file.divergence.is_some())
    }

    /// How strongly `path`, on `side`, is connected to the other side: 1 less
    /// the product of 1 less each connection's strength.
    pub fn influence(&self, side: Side, path: &str) -> f32 {
        1. - self
            .connections
            .iter()
            .filter(|connection| connection.end(side) == path)
            .map(|connection| 1. - connection.strength)
            .product::<f32>()
    }

    /// The average definition of `side`'s analyzed files, as a whole
    /// percentage.
    pub fn definition(&self, side: Side) -> Option<u32> {
        average_percent(
            self.files(side)
                .iter()
                .filter(|file| file.divergence.is_some())
                .filter_map(|file| file.definition),
        )
    }

    /// The share of `side`'s analyzed files with at least one connection, as
    /// a whole percentage.
    pub fn coverage(&self, side: Side) -> Option<u32> {
        average_percent(
            self.files(side)
                .iter()
                .filter(|file| file.divergence.is_some())
                .map(|file| {
                    let connected = self
                        .connections
                        .iter()
                        .any(|connection| connection.end(side) == file.path);
                    if connected { 1. } else { 0. }
                }),
        )
    }

    /// How many analyzed files are aligned, drifting, and diverged.
    pub fn verdict_counts(&self) -> [usize; 3] {
        let mut counts = [0; 3];
        for (_, file) in self.analyzed() {
            let verdict = Verdict::of(percent(file.divergence.unwrap_or_default()));
            counts[verdict as usize] += 1;
        }
        counts
    }

    /// The `count` best defined analyzed files, highest first, or least well
    /// defined, lowest first.
    pub fn by_definition(&self, best: bool, count: usize) -> Vec<(Side, &FileResult)> {
        let mut files: Vec<(Side, &FileResult)> = self
            .analyzed()
            .filter(|(_, file)| file.definition.is_some())
            .collect();
        files.sort_by(|a, b| {
            let (a_value, b_value) = (a.1.definition.unwrap(), b.1.definition.unwrap());
            let order = if best {
                b_value.total_cmp(&a_value)
            } else {
                a_value.total_cmp(&b_value)
            };
            order.then_with(|| a.1.path.cmp(&b.1.path))
        });
        files.truncate(count);
        files
    }

    /// The `count` analyzed files that diverge most, most first.
    pub fn most_diverged(&self, count: usize) -> Vec<(Side, &FileResult)> {
        let mut files: Vec<(Side, &FileResult)> = self.analyzed().collect();
        files.sort_by(|a, b| {
            b.1.divergence
                .unwrap()
                .total_cmp(&a.1.divergence.unwrap())
                .then_with(|| a.1.path.cmp(&b.1.path))
        });
        files.truncate(count);
        files
    }

    /// The connections of `path`, on `side`, strongest first.
    pub fn connections_of(&self, side: Side, path: &str) -> Vec<&Connection> {
        let mut connections: Vec<&Connection> = self
            .connections
            .iter()
            .filter(|connection| connection.end(side) == path)
            .collect();
        connections.sort_by(|a, b| {
            b.strength
                .total_cmp(&a.strength)
                .then_with(|| a.end(side.other()).cmp(b.end(side.other())))
        });
        connections
    }
}

/// What a divergence score means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Aligned,
    Drifting,
    Diverged,
}

impl Verdict {
    pub fn of(percent: u32) -> Self {
        match percent {
            0..20 => Verdict::Aligned,
            20..50 => Verdict::Drifting,
            _ => Verdict::Diverged,
        }
    }
}

/// How strong a connection of `strength` is.
pub fn strength_label(strength: f32) -> &'static str {
    if strength >= STRONG {
        "Strong"
    } else if strength >= MODERATE {
        "Moderate"
    } else {
        "Weak"
    }
}

/// `path` as the agents and the report write it: relative to the project,
/// with `/` between folders and no leading `./`.
fn normalize(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
}

/// The JSON reply in what an agent printed, which may have a little around it.
pub fn parse_reply(text: &str) -> Result<AgentReply> {
    let start = text
        .find('{')
        .ok_or_else(|| anyhow!("the agent's reply has no JSON in it"))?;
    let end = text
        .rfind('}')
        .ok_or_else(|| anyhow!("the agent's reply has no JSON in it"))?;
    serde_json::from_str(&text[start..=end]).context("the agent's reply isn't the JSON asked for")
}

/// Merges what the agents found for `sources` and `specs` into a report.
pub fn merge(
    sources: &[String],
    specs: &[String],
    source_reply: Option<&AgentReply>,
    spec_reply: Option<&AgentReply>,
) -> Report {
    let source_set: HashSet<&str> = sources.iter().map(String::as_str).collect();
    let spec_set: HashSet<&str> = specs.iter().map(String::as_str).collect();

    let results = |paths: &[String], reply: Option<&AgentReply>| -> Vec<FileResult> {
        let found: BTreeMap<String, &AgentFile> = reply
            .map(|reply| {
                reply
                    .files
                    .iter()
                    .map(|file| (normalize(&file.path), file))
                    .collect()
            })
            .unwrap_or_default();
        paths
            .iter()
            .map(|path| match found.get(path) {
                Some(file) => FileResult {
                    path: path.clone(),
                    divergence: Some(file.divergence.clamp(0., 1.)),
                    definition: file.definition.map(|value| value.clamp(0., 1.)),
                    summary: file.summary.trim().to_string(),
                    gaps: file
                        .gaps
                        .iter()
                        .map(|gap| gap.trim().to_string())
                        .filter(|gap| !gap.is_empty())
                        .collect(),
                },
                None => FileResult {
                    path: path.clone(),
                    ..FileResult::default()
                },
            })
            .collect()
    };

    // Each connection, keyed by its source and spec file, with what each
    // agent said of it.
    #[derive(Default)]
    struct Said {
        strengths: Vec<f32>,
        divergences: Vec<f32>,
        notes: Vec<String>,
    }
    let mut links: BTreeMap<(String, String), Said> = BTreeMap::new();
    let mut note = |source: String, spec: String, link: &AgentLink| {
        if !source_set.contains(source.as_str()) || !spec_set.contains(spec.as_str()) {
            return;
        }
        let said = links.entry((source, spec)).or_default();
        said.strengths.push(link.strength.clamp(0., 1.));
        said.divergences
            .extend(link.divergence.map(|value| value.clamp(0., 1.)));
        let text = link.note.trim();
        if !text.is_empty() && !said.notes.iter().any(|note| note == text) {
            said.notes.push(text.to_string());
        }
    };
    for file in source_reply.map_or(&[][..], |reply| &reply.files) {
        for link in &file.links {
            note(normalize(&file.path), normalize(&link.path), link);
        }
    }
    for file in spec_reply.map_or(&[][..], |reply| &reply.files) {
        for link in &file.links {
            note(normalize(&link.path), normalize(&file.path), link);
        }
    }

    let mean = |values: &[f32]| values.iter().sum::<f32>() / values.len() as f32;
    // The areas both agents named, once each, the code agent's first.
    let areas = |pick: fn(&AgentReply) -> &Vec<Area>| {
        let mut areas: Vec<Area> = Vec::new();
        for area in [source_reply, spec_reply]
            .into_iter()
            .flatten()
            .flat_map(pick)
        {
            let name = area.name.trim();
            if !name.is_empty()
                && !areas
                    .iter()
                    .any(|seen| seen.name.eq_ignore_ascii_case(name))
            {
                areas.push(Area {
                    name: name.to_string(),
                    why: area.why.trim().to_string(),
                });
            }
        }
        areas
    };
    let assessment = |reply: Option<&AgentReply>| {
        reply.map_or(String::new(), |reply| reply.assessment.trim().to_string())
    };

    Report {
        sources: results(sources, source_reply),
        specs: results(specs, spec_reply),
        connections: links
            .into_iter()
            .map(|((source, spec), said)| Connection {
                source,
                spec,
                strength: mean(&said.strengths),
                divergence: (!said.divergences.is_empty()).then(|| mean(&said.divergences)),
                notes: said.notes,
            })
            .collect(),
        code_assessment: assessment(source_reply),
        spec_assessment: assessment(spec_reply),
        well_defined: areas(|reply| &reply.well_defined),
        less_defined: areas(|reply| &reply.less_defined),
    }
}

/// A row of a file tree: a folder or a file, and how deep it is.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeRow {
    pub depth: usize,
    pub name: String,
    /// The file's path, for a file; `None` for a folder.
    pub file: Option<String>,
}

/// `paths` as a tree: a row per folder and file, folders before files, each
/// sorted by name.
pub fn tree_rows(paths: &[String]) -> Vec<TreeRow> {
    #[derive(Default)]
    struct Folder {
        folders: BTreeMap<String, Folder>,
        files: BTreeMap<String, String>,
    }
    let mut root = Folder::default();
    for path in paths {
        let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        let Some((file, folders)) = parts.split_last() else {
            continue;
        };
        let mut folder = &mut root;
        for part in folders {
            folder = folder.folders.entry(part.to_string()).or_default();
        }
        folder.files.insert(file.to_string(), path.clone());
    }
    fn walk(folder: &Folder, depth: usize, rows: &mut Vec<TreeRow>) {
        for (name, child) in &folder.folders {
            rows.push(TreeRow {
                depth,
                name: name.clone(),
                file: None,
            });
            walk(child, depth + 1, rows);
        }
        for (name, path) in &folder.files {
            rows.push(TreeRow {
                depth,
                name: name.clone(),
                file: Some(path.clone()),
            });
        }
    }
    let mut rows = Vec::new();
    walk(&root, 0, &mut rows);
    rows
}

/// The files analyzed on each side, and whether either was cut short.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Files {
    pub code_root: String,
    pub spec_root: String,
    pub sources: Vec<String>,
    pub specs: Vec<String>,
    pub sources_truncated: bool,
    pub specs_truncated: bool,
}

/// The project's source files, under its code location, and spec files, under
/// its spec location, relative to the project.
pub fn list_files(project_dir: &Path) -> Result<Files> {
    let code_root = normalize(&hidden_anchor::config_value(project_dir, "codeRoot")?);
    let spec_root = normalize(&hidden_anchor::config_value(project_dir, "root")?);
    let (sources, sources_truncated) = walk(project_dir, &code_root, |path| !is_binary(path))?;
    let (specs, specs_truncated) = walk(project_dir, &spec_root, |path| {
        path.extension().is_some_and(|ext| ext == "pi")
    })?;
    Ok(Files {
        code_root,
        spec_root,
        sources,
        specs,
        sources_truncated,
        specs_truncated,
    })
}

/// The files under `root` in `project_dir` that Git doesn't ignore and
/// `keep` keeps, sorted, at most [`MAX_FILES`] of them.
fn walk(
    project_dir: &Path,
    root: &str,
    keep: impl Fn(&Path) -> bool,
) -> Result<(Vec<String>, bool)> {
    let dir = project_dir.join(root);
    if !dir.is_dir() {
        bail!("{} isn't a folder", dir.display());
    }
    let mut files: Vec<String> = ignore::WalkBuilder::new(&dir)
        .hidden(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| entry.into_path())
        .filter(|path| keep(path))
        .filter_map(|path| {
            path.strip_prefix(project_dir)
                .ok()
                .map(|path| normalize(&path.to_string_lossy()))
        })
        .collect();
    files.sort();
    let truncated = files.len() > MAX_FILES;
    files.truncate(MAX_FILES);
    Ok((files, truncated))
}

/// Whether the file at `path` isn't text.
fn is_binary(path: &Path) -> bool {
    let mut head = [0u8; 8000];
    let Ok(mut file) = std::fs::File::open(path) else {
        return true;
    };
    let read = file.read(&mut head).unwrap_or(0);
    head[..read].contains(&0)
}

/// What an agent is asked to reply with.
const REPLY_FORMAT: &str = r#"Reply with nothing but JSON, in this form, with paths relative to the project and every number from 0 to 1:
{"assessment": "...", "wellDefined": [{"name": "...", "why": "..."}], "lessDefined": [{"name": "...", "why": "..."}], "files": [{"path": "...", "divergence": 0.0, "definition": 0.0, "summary": "...", "gaps": ["..."], "links": [{"path": "...", "strength": 0.0, "divergence": 0.0, "note": "..."}]}]}"#;

/// The prompt for the agent analyzing `side`.
pub fn prompt(side: Side, files: &Files) -> String {
    let list = |paths: &[String]| paths.join("\n");
    let intro = format!(
        "We're analyzing how far the code located in {code} and the spec located in {spec} \
         have diverged. The spec is written in Piton; its compiled Markdown is in the agent \
         directory's reference folder, such as .claude/reference, if that helps. Only read \
         files: don't change anything.",
        code = files.code_root,
        spec = files.spec_root,
    );
    let task = match side {
        Side::Source => {
            "Go through every source file listed below. For each one, say which spec files \
             drove it or had influence on it, with how strong each influence is, from 0 for \
             barely any to 1 for the file being written straight from it, how far the file \
             diverges from that spec file in particular, and a short note on what it took from \
             it. Say how far the file diverges from the spec, from 0 for doing just what the spec \
             describes to 1 for nothing in the spec accounting for it or the spec saying \
             otherwise. Say how well defined it is (definition), from 0 for the implementation \
             having filled in all of it with nothing in the spec saying how to 1 for the spec \
             defining all of it, and list the blanks the implementation filled in as gaps, each \
             in a short phrase. Sum up in one sentence how it diverges, or that it doesn't. \
             Links are to spec files from the spec list. Then, for the code as a whole, write \
             an assessment a few sentences long of how well it follows the spec, and name the \
             areas of the project that are well defined and less well defined, each with a \
             sentence on why."
        }
        Side::Spec => {
            "Go through every spec file listed below. For each one, say which source files \
             implement it, with how strongly each does, from 0 for barely at all to 1 for being \
             written straight from it, how far that source file diverges from what the file \
             describes, and a short note on what it implements. Say how far the code diverges \
             from what the file describes, from 0 for all of it being built as described to 1 \
             for none of it being built or the code doing otherwise. Say how well defined it is \
             (definition), from 0 for the file being too vague to build from to 1 for leaving \
             nothing an implementation has to guess, and list its gaps, each in a short phrase. \
             Sum up in one sentence how it diverges, or that it doesn't. Links are to source \
             files from the source list. Then, for the spec as a whole, write an assessment a \
             few sentences long of how complete and precise it is and how well the code follows \
             it, and name the areas of the project that are well defined and less well \
             defined, each with a sentence on why."
        }
    };
    let (analyzed, other) = match side {
        Side::Source => ("Source files to analyze", "Spec files"),
        Side::Spec => ("Spec files to analyze", "Source files"),
    };
    let (analyzed_files, other_files) = match side {
        Side::Source => (&files.sources, &files.specs),
        Side::Spec => (&files.specs, &files.sources),
    };
    format!(
        "{intro}\n\n{task}\n\n{REPLY_FORMAT}\n\n{analyzed}:\n{}\n\n{other}:\n{}\n",
        list(analyzed_files),
        list(other_files)
    )
}

/// Where a project's divergence reports are saved.
pub fn reports_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join("divergence")
}

/// A report as saved: when it was made, in seconds since the Unix epoch, what
/// it found, and notes about the analysis.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct SavedReport {
    pub made: u64,
    pub report: Report,
    pub notes: Vec<String>,
}

/// Now, in seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Saves `saved` in `dir`, as a JSON file named by when it was made, returning
/// the file.
pub fn save_report(dir: &Path, saved: &SavedReport) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let mut file = dir.join(format!("{}.json", saved.made));
    let mut again = 1;
    while file.exists() {
        file = dir.join(format!("{}-{again}.json", saved.made));
        again += 1;
    }
    std::fs::write(&file, serde_json::to_string_pretty(saved)?)
        .with_context(|| format!("could not save {}", file.display()))?;
    Ok(file)
}

/// The reports saved in `dir`, newest first, leaving out any that can't be
/// read.
pub fn load_reports(dir: &Path) -> Vec<SavedReport> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut reports: Vec<SavedReport> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|json| serde_json::from_str(&json).ok())
        .collect();
    reports.sort_by(|a, b| b.made.cmp(&a.made));
    reports
}

/// How long ago `then` was, at `now`, both in seconds since the Unix epoch.
pub fn ago(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then);
    let (count, unit) = match seconds {
        0..60 => return "just now".into(),
        60..3_600 => (seconds / 60, "minute"),
        3_600..86_400 => (seconds / 3_600, "hour"),
        86_400..2_592_000 => (seconds / 86_400, "day"),
        2_592_000..31_536_000 => (seconds / 2_592_000, "month"),
        _ => (seconds / 31_536_000, "year"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

/// Stops the agents of an analysis, once set.
#[derive(Default)]
pub struct Cancel {
    cancelled: AtomicBool,
}

impl Cancel {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Runs an agent in a project with a prompt, handing each line it streams to
/// the callback as it arrives, and returning its reply.
pub type RunAgent = fn(&Path, &str, &Cancel, &dyn Fn(String)) -> Result<String>;

/// Runs `piton build` in a project.
pub type Build = fn(&Path) -> Result<crate::piton_build::BuildOutcome>;

/// The reply in a line of the harness's streamed JSON, when the line is its
/// result: whether it is an error, and the result's text.
pub fn result_of(line: &str) -> Option<(bool, String)> {
    let event: serde_json::Value = serde_json::from_str(line).ok()?;
    if event.get("type")?.as_str()? != "result" {
        return None;
    }
    let is_error = event
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let result = event
        .get("result")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some((is_error, result))
}

/// Runs the harness once in `project_dir` with `prompt`, only able to read,
/// glob, and search files, keeping no session. Each line of JSON it streams
/// goes to `on_line` as it arrives; its reply is the result it finishes with.
/// Stops it if `cancel` is set while it runs.
pub fn run_agent(
    project_dir: &Path,
    prompt: &str,
    cancel: &Cancel,
    on_line: &dyn Fn(String),
) -> Result<String> {
    let mut child = Command::new("claude")
        .args([
            "-p",
            "--tools",
            "Read,Glob,Grep",
            "--no-session-persistence",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
        ])
        .current_dir(project_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run claude")?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("claude has no stdin"))?;
    let prompt = prompt.to_string();
    let writer = std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
    // Lines come through a channel, so they reach `on_line` on this thread.
    let (lines, streamed) = std::sync::mpsc::channel::<String>();
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        if let Some(stdout) = stdout {
            for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)) {
                let Ok(line) = line else { break };
                if lines.send(line).is_err() {
                    break;
                }
            }
        }
    });
    let stderr = child.stderr.take();
    let stderr = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut stderr) = stderr {
            stderr.read_to_string(&mut text).ok();
        }
        text
    });
    let child = Arc::new(Mutex::new(child));
    let mut result = None;
    let mut take = |line: String| {
        if let Some(found) = result_of(&line) {
            result = Some(found);
        }
        if !line.trim().is_empty() {
            on_line(line);
        }
    };
    let status = loop {
        while let Ok(line) = streamed.try_recv() {
            take(line);
        }
        if cancel.is_cancelled() {
            kill(&child);
            bail!("stopped");
        }
        if let Some(status) = child.lock().unwrap().try_wait()? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    writer.join().ok();
    reader.join().ok();
    while let Ok(line) = streamed.try_recv() {
        take(line);
    }
    let stderr = stderr.join().unwrap_or_default();
    match result {
        Some((false, reply)) => Ok(reply),
        Some((true, reply)) => bail!("{}", reply.trim()),
        None if !status.success() => bail!("{}", stderr.trim()),
        None => bail!("the agent finished without a result"),
    }
}

fn kill(child: &Arc<Mutex<Child>>) {
    let mut child = child.lock().unwrap();
    child.kill().ok();
    child.wait().ok();
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        AgentReply, Side, Verdict, list_files, merge, normalize, parse_reply, strength_label,
        tree_rows,
    };

    fn reply(json: &str) -> AgentReply {
        parse_reply(json).unwrap()
    }

    /// A reply is read even with text around its JSON, and one that isn't the
    /// JSON asked for says so.
    #[test]
    fn replies_are_parsed() {
        let parsed = reply(
            "Here you go:\n{\"files\": [{\"path\": \"./src/a.rs\", \"divergence\": 0.5, \
             \"links\": [{\"path\": \"spec/a.pi\", \"strength\": 0.9}]}]}\nDone.",
        );
        assert_eq!(parsed.files[0].links[0].strength, 0.9);
        assert!(parse_reply("no json").is_err());
        assert!(parse_reply("{\"nope\": 1}").is_err());
        assert_eq!(normalize("./src\\a.rs"), "src/a.rs");
    }

    /// Connections from both sides are joined by their files and averaged;
    /// ones to files outside either list are dropped; files an agent left out
    /// aren't analyzed and don't count towards the score.
    #[test]
    fn replies_merge_into_connections_and_a_score() {
        let sources = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        let specs = vec!["spec/a.pi".to_string(), "spec/b.pi".to_string()];
        let source_reply = reply(
            r#"{"files": [
                {"path": "src/a.rs", "divergence": 0.2, "summary": "Mostly as described.",
                 "links": [{"path": "spec/a.pi", "strength": 1.0, "note": "the layout"},
                           {"path": "spec/nowhere.pi", "strength": 0.5}]}
            ]}"#,
        );
        let spec_reply = reply(
            r#"{"files": [
                {"path": "spec/a.pi", "divergence": 0.4,
                 "links": [{"path": "src/a.rs", "strength": 0.6, "note": "the layout"},
                           {"path": "src/b.rs", "strength": 0.2, "note": "a little"}]},
                {"path": "spec/b.pi", "divergence": 1.5}
            ]}"#,
        );
        let report = merge(&sources, &specs, Some(&source_reply), Some(&spec_reply));

        assert_eq!(report.sources[0].divergence, Some(0.2));
        assert_eq!(report.sources[1].divergence, None, "b.rs was analyzed");
        assert_eq!(
            report.specs[1].divergence,
            Some(1.),
            "divergence isn't clamped"
        );
        assert_eq!(report.connections.len(), 2);
        let a = &report.connections_of(Side::Source, "src/a.rs")[0];
        assert_eq!((a.spec.as_str(), a.strength), ("spec/a.pi", 0.8));
        assert_eq!(a.notes, ["the layout"]);
        let of_spec = report.connections_of(Side::Spec, "spec/a.pi");
        assert_eq!(
            of_spec
                .iter()
                .map(|c| c.source.as_str())
                .collect::<Vec<_>>(),
            ["src/a.rs", "src/b.rs"],
            "strongest first"
        );
        // (0.2 + 0.4 + 1.0) / 3
        assert_eq!(report.score(), Some(53));
        assert_eq!(report.alignment(), Some(47));
        assert_eq!(merge(&sources, &specs, None, None).score(), None);
    }

    #[test]
    fn scores_and_strengths_are_named() {
        assert_eq!(Verdict::of(0), Verdict::Aligned);
        assert_eq!(Verdict::of(19), Verdict::Aligned);
        assert_eq!(Verdict::of(20), Verdict::Drifting);
        assert_eq!(Verdict::of(50), Verdict::Diverged);
        assert_eq!(strength_label(0.9), "Strong");
        assert_eq!(strength_label(0.4), "Moderate");
        assert_eq!(strength_label(0.1), "Weak");
    }

    /// The reply is the result the stream ends with.
    #[test]
    fn results_are_found_in_the_stream() {
        use super::result_of;
        assert_eq!(result_of(r#"{"type":"assistant","message":{}}"#), None);
        assert_eq!(result_of("not json"), None);
        assert_eq!(
            result_of(r#"{"type":"result","is_error":false,"result":"{\"files\": []}"}"#),
            Some((false, r#"{"files": []}"#.to_string()))
        );
        assert_eq!(
            result_of(r#"{"type":"result","is_error":true,"result":"overloaded"}"#),
            Some((true, "overloaded".to_string()))
        );
    }

    /// Each file's influence, definition, and gaps, each connection's
    /// divergence, and the assessments and areas both agents gave.
    #[test]
    fn replies_merge_into_measures_and_an_assessment() {
        use super::Side;
        let sources = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        let specs = vec!["spec/a.pi".to_string(), "spec/b.pi".to_string()];
        let source_reply = reply(
            r#"{"assessment": " Follows it closely. ",
                "wellDefined": [{"name": "Layout", "why": "Every size is given."}],
                "lessDefined": [{"name": "Errors", "why": "Nothing says what shows."}],
                "files": [
                {"path": "src/a.rs", "divergence": 0.2, "definition": 0.9, "gaps": [" colours ", ""],
                 "links": [{"path": "spec/a.pi", "strength": 0.5, "divergence": 0.2},
                           {"path": "spec/b.pi", "strength": 0.5}]},
                {"path": "src/b.rs", "divergence": 0.6, "definition": 0.3}
            ]}"#,
        );
        let spec_reply = reply(
            r#"{"assessment": "Has gaps.",
                "wellDefined": [{"name": "layout", "why": "Again."}],
                "lessDefined": [{"name": "Saving"}],
                "files": [
                {"path": "spec/a.pi", "divergence": 0.4, "definition": 0.5,
                 "links": [{"path": "src/a.rs", "strength": 0.5, "divergence": 0.4}]}
            ]}"#,
        );
        let report = merge(&sources, &specs, Some(&source_reply), Some(&spec_reply));

        assert_eq!(report.sources[0].gaps, ["colours"]);
        // 1 - (1 - 0.5) * (1 - 0.5)
        assert!((report.influence(Side::Source, "src/a.rs") - 0.75).abs() < 1e-4);
        assert_eq!(report.influence(Side::Source, "src/b.rs"), 0.);
        let a = &report.connections_of(Side::Source, "src/a.rs")[0];
        assert!((a.divergence.unwrap() - 0.3).abs() < 1e-4);
        assert_eq!(
            report.connections_of(Side::Source, "src/a.rs")[1].divergence,
            None
        );
        // (0.9 + 0.3) / 2, and only spec/a.pi was analyzed.
        assert_eq!(report.definition(Side::Source), Some(60));
        assert_eq!(report.definition(Side::Spec), Some(50));
        assert_eq!(report.coverage(Side::Source), Some(50));
        assert_eq!(report.coverage(Side::Spec), Some(100));
        // 0.2 and 0.4 drifting; 0.6 diverged.
        assert_eq!(report.verdict_counts(), [0, 2, 1]);
        let paths = |files: Vec<(Side, &super::FileResult)>| {
            files
                .into_iter()
                .map(|(_, file)| file.path.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            paths(report.by_definition(true, 2)),
            ["src/a.rs", "spec/a.pi"]
        );
        assert_eq!(paths(report.by_definition(false, 1)), ["src/b.rs"]);
        assert_eq!(paths(report.most_diverged(2)), ["src/b.rs", "spec/a.pi"]);
        assert_eq!(report.code_assessment, "Follows it closely.");
        assert_eq!(report.spec_assessment, "Has gaps.");
        assert_eq!(
            report
                .well_defined
                .iter()
                .map(|area| area.name.as_str())
                .collect::<Vec<_>>(),
            ["Layout"],
            "named once"
        );
        assert_eq!(report.less_defined.len(), 2);
    }

    /// Reports are saved and read back newest first; one that can't be read is
    /// left out.
    #[test]
    fn reports_are_saved_and_loaded() {
        use super::{SavedReport, ago, load_reports, save_report};
        let dir = std::env::temp_dir().join(format!("suspense-divergence-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        assert!(load_reports(&dir).is_empty());
        let report = merge(
            &["src/a.rs".to_string()],
            &[],
            Some(&reply(
                r#"{"assessment": "Fine.", "files": [{"path": "src/a.rs", "divergence": 0.5}]}"#,
            )),
            None,
        );
        let older = SavedReport {
            made: 100,
            report: report.clone(),
            notes: vec!["A note".into()],
        };
        let newer = SavedReport {
            made: 200,
            report,
            notes: Vec::new(),
        };
        save_report(&dir, &older).unwrap();
        save_report(&dir, &newer).unwrap();
        // Made in the same second, it is saved beside the other.
        save_report(&dir, &newer).unwrap();
        std::fs::write(dir.join("broken.json"), "{").unwrap();
        let loaded = load_reports(&dir);
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[2], older);
        assert_eq!(loaded[0], newer);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(ago(100, 130), "just now");
        assert_eq!(ago(0, 60), "1 minute ago");
        assert_eq!(ago(0, 7_200), "2 hours ago");
        assert_eq!(ago(0, 86_400 * 3), "3 days ago");
    }

    /// Folders come before files, each sorted, indented by depth.
    #[test]
    fn paths_become_a_tree() {
        let paths = ["src/main.rs", "src/ui/b.rs", "src/ui/a.rs", "build.rs"]
            .map(String::from)
            .to_vec();
        let rows: Vec<(usize, String, bool)> = tree_rows(&paths)
            .into_iter()
            .map(|row| (row.depth, row.name, row.file.is_some()))
            .collect();
        let rows: Vec<(usize, &str, bool)> = rows
            .iter()
            .map(|(depth, name, file)| (*depth, name.as_str(), *file))
            .collect();
        assert_eq!(
            rows,
            [
                (0, "src", false),
                (1, "ui", false),
                (2, "a.rs", true),
                (2, "b.rs", true),
                (1, "main.rs", true),
                (0, "build.rs", true),
            ]
        );
    }

    /// This project's own files: text source files under src, and .pi files
    /// under spec.
    #[test]
    fn lists_this_projects_files() {
        let files = list_files(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert_eq!(
            (files.code_root.as_str(), files.spec_root.as_str()),
            ("src", "spec")
        );
        assert!(files.sources.contains(&"src/divergence.rs".to_string()));
        assert!(files.sources.iter().all(|path| path.starts_with("src/")));
        assert!(
            files
                .specs
                .contains(&"spec/scope/divergence/index.pi".to_string())
        );
        assert!(files.specs.iter().all(|path| path.ends_with(".pi")));
    }
}
