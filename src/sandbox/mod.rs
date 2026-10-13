//! The sandbox, as the SandboxScope says: native, per-command filesystem
//! access control for the harness runs the application starts, enforced by
//! the operating system for the run and everything it starts.
//!
//! A [`Policy`] says what a run may read and change; it is [`compile`]d into
//! an allow-list of [`Rule`]s a backend can enforce, every path resolved,
//! grants that hold a denied path expanded around it, and contradictions
//! refused. A [`Profile`] builds the policy for a kind of run from the
//! project's [`Classification`]. [`prepare`] makes a run's own temporary
//! directory, its filtered git repository, and its piton channel, and
//! [`Prepared::apply`] puts the restriction on the run's command, so it is
//! in force before the harness's program runs. Where it can't be, the run
//! never starts: it fails closed.
//!
//! See `SANDBOX.md` beside this module for what each backend enforces, what
//! remains outside it, and how it was chosen.

use std::collections::{BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

pub mod channel;
mod git;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
pub mod reach;
#[cfg(windows)]
pub mod windows;

/// What a grant lets a run do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Access {
    /// Read, list, and execute from; never change.
    ReadOnly,
    /// Also create in, change, rename within, and delete from.
    ReadWrite,
}

/// What a run may read and change, before it is compiled.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Policy {
    /// The project directory, the run's working directory: within it,
    /// everything is denied unless a grant reaches it.
    pub project: PathBuf,
    /// Paths granted, in the project or outside it.
    pub grants: Vec<(PathBuf, Access)>,
    /// Paths in the project no grant may reach.
    pub denied: Vec<PathBuf>,
    /// Paths in the project a wider grant doesn't reach, though a grant of
    /// its own, inside them, may: the project's data, of which a run is
    /// granted only its own files.
    pub excluded: Vec<PathBuf>,
}

impl Policy {
    pub fn new(project: impl Into<PathBuf>) -> Self {
        Self {
            project: project.into(),
            ..Self::default()
        }
    }

    pub fn read_only(mut self, path: impl Into<PathBuf>) -> Self {
        self.grants.push((path.into(), Access::ReadOnly));
        self
    }

    pub fn read_write(mut self, path: impl Into<PathBuf>) -> Self {
        self.grants.push((path.into(), Access::ReadWrite));
        self
    }

    pub fn deny(mut self, path: impl Into<PathBuf>) -> Self {
        self.denied.push(path.into());
        self
    }

    pub fn exclude(mut self, path: impl Into<PathBuf>) -> Self {
        self.excluded.push(path.into());
        self
    }
}

/// One path of the allow-list a backend enforces, resolved.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rule {
    pub path: PathBuf,
    pub access: Access,
    /// What it was as the policy was compiled, its device and inode, so a
    /// backend granting it by an open handle can tell it wasn't swapped
    /// since.
    pub id: Option<(u64, u64)>,
}

/// The device and inode of `path`, whatever its links.
#[cfg(unix)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_: &Path) -> Option<(u64, u64)> {
    None
}

impl Rule {
    fn new(path: PathBuf, access: Access) -> Self {
        Self {
            id: file_id(&path),
            path,
            access,
        }
    }
}

/// A policy compiled to what a backend enforces: only an allow-list, every
/// path resolved, nothing in it reaching a denied path.
#[derive(Clone, Debug, PartialEq)]
pub struct Compiled {
    pub project: PathBuf,
    pub rules: Vec<Rule>,
    /// The denied paths, resolved, which the backends that can deny, as
    /// Seatbelt, deny outright too.
    pub denied: Vec<PathBuf>,
    /// The run's own name, `Suspense.<profile>.<run id>`, which Windows
    /// names its AppContainer by; empty until it is prepared.
    pub label: String,
}

/// Why a run's sandbox couldn't be applied, so the run never starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    /// What it says, in plain words, naming the capability or path at fault.
    pub message: String,
    /// The backend's own error, for the Details disclosure.
    pub details: String,
}

/// What a run whose sandbox couldn't be applied says first.
pub const REFUSED: &str = "This run's sandbox couldn't be applied";

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{REFUSED}, so it never started: {}", self.message)?;
        if !self.details.is_empty() {
            write!(f, "\n\n{}", self.details)?;
        }
        Ok(())
    }
}

impl std::error::Error for Refused {}

fn refused(message: impl Into<String>) -> Refused {
    Refused {
        message: message.into(),
        details: String::new(),
    }
}

/// `path` made absolute against `base`, with `.` and `..` taken out by
/// name, before any link is followed.
fn absolute(path: &Path, base: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

/// `path` resolved: absolute, `..` taken out, every link followed. None
/// where it isn't there.
fn resolve(path: &Path, base: &Path) -> Option<PathBuf> {
    dunce::canonicalize(absolute(path, base)).ok()
}

/// How deep, and how many entries, the checks for links into a denied path
/// look through a grant.
const CHECK_LIMIT: usize = 50_000;

/// Folders a check for links doesn't look into: build output and
/// dependencies, which the kernel still holds to the policy at every access.
const UNCHECKED: [&str; 3] = ["target", "node_modules", ".git"];

/// Compiles `policy` to the allow-list a backend enforces, as the
/// SandboxScope's policy says, or refuses it as contradictory. Blocking: it
/// looks through the grants for links into denied paths.
pub fn compile(policy: &Policy) -> Result<Compiled, Refused> {
    let project = dunce::canonicalize(&policy.project).map_err(|err| Refused {
        message: format!(
            "the project directory {} can't be found",
            policy.project.display()
        ),
        details: err.to_string(),
    })?;
    let denied: Vec<PathBuf> = policy
        .denied
        .iter()
        .filter_map(|path| resolve(path, &project))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for path in &denied {
        if !path.starts_with(&project) {
            return Err(refused(format!(
                "the denied path {} is outside the project",
                path.display()
            )));
        }
    }
    // Every grant resolved, the widest access where one is granted twice;
    // one that isn't there is left out.
    let mut grants: Vec<(PathBuf, Access)> = Vec::new();
    for (path, access) in &policy.grants {
        let Some(resolved) = resolve(path, &project) else {
            continue;
        };
        if let Some(denied) = denied.iter().find(|denied| resolved.starts_with(denied)) {
            return Err(refused(format!(
                "{} is granted, but it is {} denied path {}",
                path.display(),
                if resolved == *denied {
                    "the"
                } else {
                    "inside the"
                },
                denied.display()
            )));
        }
        match grants.iter_mut().find(|(known, _)| *known == resolved) {
            Some((_, known)) => *known = (*known).max(*access),
            None => grants.push((resolved, *access)),
        }
    }
    let excluded: Vec<PathBuf> = policy
        .excluded
        .iter()
        .filter_map(|path| resolve(path, &project))
        .collect();
    // What each grant is expanded around: the denied and excluded paths and
    // narrower grants of less access within it; for a grant outside the
    // project that holds it, the project too.
    let mut rules = Vec::new();
    for (path, access) in &grants {
        let mut around: Vec<PathBuf> = denied
            .iter()
            .chain(&excluded)
            .filter(|cut| cut.starts_with(path) && *cut != path)
            .cloned()
            .collect();
        around.extend(
            grants
                .iter()
                .filter(|(other, other_access)| {
                    other.starts_with(path) && other != path && other_access < access
                })
                .map(|(other, _)| other.clone()),
        );
        if !path.starts_with(&project) && project.starts_with(path) {
            around.push(project.clone());
        }
        expand(path, *access, &around, &denied, &mut rules)?;
    }
    rules.sort();
    rules.dedup();
    check_links(&project, &rules, &denied)?;
    Ok(Compiled {
        project,
        rules,
        denied,
        label: String::new(),
    })
}

/// Grants `path` with `access`, or, where something in `around` lies within
/// it, each of its entries but the ones leading to those, level by level.
fn expand(
    path: &Path,
    access: Access,
    around: &[PathBuf],
    denied: &[PathBuf],
    rules: &mut Vec<Rule>,
) -> Result<(), Refused> {
    let inside: Vec<&PathBuf> = around
        .iter()
        .filter(|cut| cut.starts_with(path) && cut.as_path() != path)
        .collect();
    if inside.is_empty() {
        rules.push(Rule::new(path.to_path_buf(), access));
        return Ok(());
    }
    let entries = std::fs::read_dir(path).map_err(|err| Refused {
        message: format!("{} can't be listed to grant what it holds", path.display()),
        details: err.to_string(),
    })?;
    for entry in entries.flatten() {
        let entry = entry.path();
        if around.contains(&entry) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&entry) else {
            continue;
        };
        // A link is granted as what it resolves to, never into a denied
        // path.
        let target = if meta.file_type().is_symlink() {
            match dunce::canonicalize(&entry) {
                Ok(target) => target,
                Err(_) => continue,
            }
        } else {
            entry.clone()
        };
        if let Some(denied) = denied.iter().find(|denied| target.starts_with(denied)) {
            return Err(refused(format!(
                "{} links to the denied path {}",
                entry.display(),
                denied.display()
            )));
        }
        if inside.iter().any(|cut| cut.starts_with(&entry)) {
            expand(&entry, access, around, denied, rules)?;
        } else {
            rules.push(Rule::new(target, access));
        }
    }
    Ok(())
}

/// The identity of a file: its device and inode, or its file id.
#[cfg(unix)]
fn identity(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    (meta.nlink() > 1).then(|| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn identity(_: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Every entry beneath `root`, but what [`UNCHECKED`] names, up to
/// [`CHECK_LIMIT`] of them.
fn walk(
    root: &Path,
    seen: &mut usize,
    each: &mut dyn FnMut(&Path, &std::fs::Metadata) -> Result<(), Refused>,
) -> Result<(), Refused> {
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            *seen += 1;
            if *seen > CHECK_LIMIT {
                return Ok(());
            }
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            each(&path, &meta)?;
            if meta.is_dir()
                && !UNCHECKED
                    .iter()
                    .any(|name| path.file_name().is_some_and(|file| file == *name))
            {
                folders.push(path);
            }
        }
    }
    Ok(())
}

/// Refuses a policy whose grants in the project reach a denied file by a
/// link, or hold another name, a hard link, of one.
fn check_links(project: &Path, rules: &[Rule], denied: &[PathBuf]) -> Result<(), Refused> {
    // The identities of denied files with more than one name.
    let mut linked: HashSet<(u64, u64)> = HashSet::new();
    let mut seen = 0;
    for path in denied {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            continue;
        };
        linked.extend(identity(&meta));
        if meta.is_dir() {
            walk(path, &mut seen, &mut |_, meta| {
                if meta.is_file() {
                    linked.extend(identity(meta));
                }
                Ok(())
            })?;
        }
    }
    let mut seen = 0;
    for rule in rules.iter().filter(|rule| rule.path.starts_with(project)) {
        let mut check = |path: &Path, meta: &std::fs::Metadata| {
            if meta.file_type().is_symlink()
                && let Ok(target) = dunce::canonicalize(path)
                && let Some(denied) = denied.iter().find(|denied| target.starts_with(denied))
            {
                return Err(refused(format!(
                    "{} links to the denied path {}",
                    path.display(),
                    denied.display()
                )));
            }
            if meta.is_file()
                && let Some(id) = identity(meta)
                && linked.contains(&id)
            {
                return Err(refused(format!(
                    "{} is another name, a hard link, for a denied file",
                    path.display()
                )));
            }
            Ok(())
        };
        let Ok(meta) = std::fs::symlink_metadata(&rule.path) else {
            continue;
        };
        check(&rule.path, &meta)?;
        if meta.is_dir() {
            walk(&rule.path, &mut seen, &mut check)?;
        }
    }
    Ok(())
}

/// The platform's backend, and what it enforces here, as the settings and
/// the welcome checks say; or why it can't run, so no run will.
pub fn status() -> Result<String, Refused> {
    #[cfg(target_os = "linux")]
    return linux::status();
    #[cfg(target_os = "macos")]
    return macos::status();
    #[cfg(windows)]
    return windows::status();
    #[allow(unreachable_code)]
    Err(refused("this platform has no sandbox backend"))
}

/// The backend's name, as the settings show it.
pub fn backend_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "Landlock"
    } else if cfg!(target_os = "macos") {
        "Seatbelt"
    } else if cfg!(windows) {
        "AppContainer"
    } else {
        "None"
    }
}

/// Where the sandbox's documentation is.
pub const DOCUMENTATION: &str =
    "https://github.com/piton-lang/suspense/blob/main/src/sandbox/SANDBOX.md";

/// The kind of run a profile is for, as the SandboxScope's profiles say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProfileKind {
    /// A Code task, a Chain prompt's code step.
    Code,
    /// A Spec task, a Chain prompt's spec step and spec follow-up.
    Spec,
    /// A question, and the application's own runs that read the project.
    Question,
    /// A Freeform task, which may change either.
    Freeform,
    /// The application's small runs that read nothing of the project.
    Bare,
}

impl ProfileKind {
    pub fn label(self) -> &'static str {
        match self {
            ProfileKind::Code => "Code",
            ProfileKind::Spec => "Spec",
            ProfileKind::Question => "Question",
            ProfileKind::Freeform => "Freeform",
            ProfileKind::Bare => "Bare",
        }
    }

    /// The profile a prompt sent in `mode` runs under.
    pub fn of(mode: Option<crate::chat_input::SendMode>) -> Self {
        use crate::chat_input::SendMode;
        match mode {
            Some(SendMode::Code) => ProfileKind::Code,
            Some(SendMode::Spec | SendMode::Both) => ProfileKind::Spec,
            Some(SendMode::Ask) => ProfileKind::Question,
            Some(SendMode::Freeform) | None => ProfileKind::Freeform,
        }
    }

    /// Whether git is given to it filtered, never the project's own.
    fn filtered_git(self) -> bool {
        !matches!(self, ProfileKind::Freeform)
    }
}

/// The project's classification of its paths as code or spec, as
/// `.suspense/sandbox.json` keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Classification {
    /// Paths in the project that are code, relative to it.
    #[serde(default)]
    pub code_roots: Vec<String>,
    #[serde(default)]
    pub spec_roots: Vec<String>,
    /// Patterns classing files anywhere as code or spec, as `**/*.pi`.
    #[serde(default)]
    pub code_patterns: Vec<String>,
    #[serde(default)]
    pub spec_patterns: Vec<String>,
    /// The project's own levels for each mode, by area, where they differ
    /// from the profile's default, as the settings' grid sets them.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub levels: reach::Levels,
    /// The areas the project added to the grid, in the order added.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub areas: Vec<AddedArea>,
}

/// An area the project added to the settings' grid, as the SandboxScope's
/// classification says: its name, the folders and patterns it covers, and
/// its own level in each mode where it has one, Other files' otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedArea {
    pub name: String,
    #[serde(default)]
    pub roots: Vec<String>,
    #[serde(default)]
    pub patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub levels: std::collections::BTreeMap<String, reach::Level>,
}

/// The files in the project at `project_dir` any of `patterns` matches.
fn matching(patterns: &[String], project_dir: &Path) -> Vec<PathBuf> {
    if patterns.is_empty() {
        return Vec::new();
    }
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        if let Ok(glob) = globset::Glob::new(pattern) {
            builder.add(glob);
        }
    }
    let Ok(set) = builder.build() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut seen = 0;
    walk(project_dir, &mut seen, &mut |path, meta| {
        if meta.is_file() && set.is_match(relative(project_dir, path)) {
            found.push(path.to_path_buf());
        }
        Ok(())
    })
    .ok();
    found
}

/// Where a project's classification is kept.
pub fn classification_file(project_dir: &Path) -> PathBuf {
    project_dir
        .join(crate::hidden_anchor::APP_DIR)
        .join("sandbox.json")
}

/// `path`, in the project at `project_dir`, as the classification names it.
fn relative(project_dir: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(project_dir).unwrap_or(path);
    let text = relative.to_string_lossy().replace('\\', "/");
    if text.is_empty() { ".".into() } else { text }
}

impl Classification {
    /// The classification the project's locations give: its code location
    /// its only code root, its spec location its only spec root.
    pub fn from_locations(project_dir: &Path) -> Self {
        let locations = crate::project_tree::Locations::read(project_dir);
        Self {
            code_roots: vec![relative(
                project_dir,
                &locations.code.unwrap_or_else(|| project_dir.to_path_buf()),
            )],
            spec_roots: locations
                .spec
                .iter()
                .map(|spec| relative(project_dir, spec))
                .collect(),
            ..Self::default()
        }
    }

    /// The project's own, or with none saved, the one its locations give.
    pub fn load(project_dir: &Path) -> Self {
        std::fs::read_to_string(classification_file(project_dir))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_else(|| Self::from_locations(project_dir))
    }

    pub fn save(&self, project_dir: &Path) -> std::io::Result<()> {
        let file = classification_file(project_dir);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(file, text + "\n")
    }

    /// The files in the project its patterns class as code, and as spec.
    fn patterned(&self, project_dir: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
        (
            matching(&self.code_patterns, project_dir),
            matching(&self.spec_patterns, project_dir),
        )
    }

    /// What the added area `area` covers, absolute: its folders and the
    /// files its patterns match.
    pub fn area_paths(area: &AddedArea, project_dir: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = area
            .roots
            .iter()
            .map(|root| absolute(Path::new(root), project_dir))
            .collect();
        paths.extend(matching(&area.patterns, project_dir));
        paths
    }

    /// The area that already has what the added area at `index` covers by
    /// `entry`, a folder or, where `pattern`, a pattern, as the settings
    /// mark it: Code, Spec source, Compiled reference, Suspense data, or
    /// another added area; none where nothing does.
    pub fn taken_by(
        &self,
        index: usize,
        entry: &str,
        pattern: bool,
        project_dir: &Path,
        reference: &Path,
    ) -> Option<String> {
        let (code, spec) = self.resolve(project_dir).unwrap_or_default();
        let mut others: Vec<(String, Vec<PathBuf>)> = vec![
            ("Code".into(), code),
            ("Spec source".into(), spec),
            ("Compiled reference".into(), vec![reference.to_path_buf()]),
            (
                "Suspense data".into(),
                vec![project_dir.join(crate::hidden_anchor::APP_DIR)],
            ),
        ];
        for (other, area) in self.areas.iter().enumerate() {
            if other != index {
                others.push((area.name.clone(), Self::area_paths(area, project_dir)));
            }
        }
        let covered = if pattern {
            matching(&[entry.to_string()], project_dir)
        } else {
            vec![absolute(Path::new(entry), project_dir)]
        };
        others
            .into_iter()
            .find(|(_, paths)| {
                covered.iter().any(|path| {
                    paths.iter().any(|other| {
                        path.starts_with(other) || (!pattern && other.starts_with(path))
                    })
                })
            })
            .map(|(name, _)| name)
    }

    /// The first added area that overlaps another area, said as runs are
    /// refused for it; none where none does.
    pub fn area_conflict(&self, project_dir: &Path, reference: &Path) -> Option<String> {
        for (index, area) in self.areas.iter().enumerate() {
            let entries = area
                .roots
                .iter()
                .map(|root| (root, false))
                .chain(area.patterns.iter().map(|pattern| (pattern, true)));
            for (entry, pattern) in entries {
                if let Some(taken) = self.taken_by(index, entry, pattern, project_dir, reference) {
                    return Some(format!(
                        "the area {} covers {entry}, which {taken} already has; runs won't start until it doesn't",
                        area.name
                    ));
                }
            }
        }
        None
    }

    /// The paths classed as code and as spec, absolute; or the path classed
    /// as both, which no run starts with until it is resolved.
    pub fn resolve(&self, project_dir: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>), Refused> {
        let roots = |roots: &[String]| -> Vec<PathBuf> {
            roots
                .iter()
                .map(|root| absolute(Path::new(root), project_dir))
                .collect()
        };
        let (mut code, mut spec) = (roots(&self.code_roots), roots(&self.spec_roots));
        let (code_files, spec_files) = self.patterned(project_dir);
        code.extend(code_files);
        spec.extend(spec_files);
        // A code root that holds a spec root, as a code location that is the
        // whole project, is code but for the spec within it; the same the
        // other way round. Only the same path in both is both.
        for path in &code {
            if spec.contains(path) {
                return Err(refused(format!(
                    "{} is classed as both code and spec; runs won't start until it is one or the other",
                    relative(project_dir, path)
                )));
            }
        }
        Ok((code, spec))
    }

    /// The path classed as both, if any, as the settings mark it.
    pub fn conflict(&self, project_dir: &Path) -> Option<String> {
        self.resolve(project_dir)
            .err()
            .map(|refused| refused.message)
    }
}

/// What a run is, for its profile.
#[derive(Clone, Debug, Default)]
pub struct RunFiles {
    /// Its prompt's attached files, each granted on its own, read only.
    pub attached: Vec<PathBuf>,
    /// Its understanding file, read-write.
    pub understanding: Option<PathBuf>,
}

/// The policy a run of `kind` in the project at `project_dir`, by `agent`,
/// runs under, as the SandboxScope's profiles say; refused where the
/// project's classification is contradictory.
pub fn profile(
    kind: ProfileKind,
    agent: crate::agent::Agent,
    project_dir: &Path,
    files: &RunFiles,
) -> Result<Policy, Refused> {
    let mut policy = Policy::new(project_dir);
    let data = project_dir.join(crate::hidden_anchor::APP_DIR);
    let git = project_dir.join(".git");
    let harness_dir = project_dir.join(agent.directory());
    let config = project_dir.join(crate::project_directory::CONFIG_FILE_NAME);
    let classification = Classification::load(project_dir);
    let (code, spec) = match kind {
        ProfileKind::Bare => (Vec::new(), Vec::new()),
        // A Freeform run reaches either way; its areas only matter to the
        // levels the project sets for it.
        ProfileKind::Freeform => classification.resolve(project_dir).unwrap_or_default(),
        _ => classification.resolve(project_dir)?,
    };
    // A root that holds one of the other class is that class but for it.
    let holding = |roots: &[PathBuf], others: &[PathBuf]| -> Vec<PathBuf> {
        roots
            .iter()
            .filter(|root| !others.iter().any(|other| other.starts_with(root)))
            .cloned()
            .collect()
    };
    match kind {
        ProfileKind::Code => {
            for root in &code {
                policy = policy.read_write(root);
            }
            // The build's output.
            for output in ["target", "node_modules", "build", "dist"] {
                let output = project_dir.join(output);
                if !spec.iter().any(|spec| spec.starts_with(&output)) {
                    policy = policy.read_write(output);
                }
            }
            policy = policy.read_only(&harness_dir);
            for denied in holding(&spec, &code) {
                policy = policy.deny(denied);
            }
        }
        ProfileKind::Spec => {
            for root in &spec {
                policy = policy.read_write(root);
            }
            policy = policy
                .read_write(&harness_dir)
                .read_write(project_dir.join(".piton"))
                .read_only(&config);
            let reads_code = crate::project_settings::spec_reads_code(project_dir);
            let reads_project =
                reads_code && crate::project_settings::spec_reads_project(project_dir);
            if reads_project {
                policy = policy.read_only(project_dir);
            }
            if reads_code {
                for root in &code {
                    policy = policy.read_only(root);
                }
            } else {
                for denied in holding(&code, &spec) {
                    policy = policy.deny(denied);
                }
            }
        }
        ProfileKind::Question => {
            for root in code.iter().chain(&spec) {
                policy = policy.read_only(root);
            }
            policy = policy.read_only(&harness_dir).read_only(&config);
        }
        ProfileKind::Freeform => {
            policy = policy.read_write(project_dir);
        }
        ProfileKind::Bare => {}
    }
    if kind != ProfileKind::Bare {
        // Guidance at the project's top, as the build writes it.
        for guidance in ["CLAUDE.md", "AGENTS.md", ".mcp.json"] {
            let file = project_dir.join(guidance);
            if !spec.contains(&file) && !code.contains(&file) {
                policy = policy.read_only(file);
            }
        }
        policy = policy
            .read_only(crate::piton_fluency::file(project_dir))
            .read_only(crate::suspense_fluency::file(project_dir));
        for file in &files.attached {
            policy = policy.read_only(file);
        }
        if let Some(file) = &files.understanding {
            if !file.exists() {
                std::fs::write(file, "").ok();
            }
            policy = policy.read_write(file);
        }
        // Never the project's data, but for the files above; nor its git,
        // but for a Freeform run, which is given it.
        policy = policy.exclude(&data);
        if kind != ProfileKind::Freeform {
            policy = policy.deny(&git);
        }
        // The project's own areas and levels, over the defaults; an added
        // area overlapping another refuses.
        let reference = harness_dir.join("reference");
        if let Some(why) = classification.area_conflict(project_dir, &reference) {
            return Err(refused(why));
        }
        let mut areas = vec![
            (reach::Area::Code, code.clone()),
            (reach::Area::SpecSource, spec.clone()),
            (reach::Area::Reference, vec![reference]),
            (reach::Area::ProjectData, vec![data.clone()]),
        ];
        for (index, area) in classification.areas.iter().enumerate() {
            areas.push((
                reach::Area::Added(index),
                Classification::area_paths(area, project_dir),
            ));
        }
        // The files a run is given on its own, kept whatever the levels.
        let own: Vec<PathBuf> = files
            .attached
            .iter()
            .chain(&files.understanding)
            .cloned()
            .chain([
                crate::piton_fluency::file(project_dir),
                crate::suspense_fluency::file(project_dir),
            ])
            .collect();
        reach::apply(&mut policy, kind, &classification, &areas, &own)?;
    }
    for (path, access) in outside_grants(agent) {
        policy.grants.push((path, access));
    }
    Ok(policy)
}

/// What outside the project every run is granted: the system and the
/// toolchains, read only; the harness's own folders and the toolchains'
/// caches, read-write. Never the user's whole home.
pub fn outside_grants(agent: crate::agent::Agent) -> Vec<(PathBuf, Access)> {
    let mut grants = Vec::new();
    let ro = Access::ReadOnly;
    let rw = Access::ReadWrite;
    #[cfg(unix)]
    {
        for system in [
            "/usr",
            "/bin",
            "/sbin",
            "/lib",
            "/lib32",
            "/lib64",
            "/libx32",
            "/etc",
            "/opt",
            "/nix",
            "/snap",
            "/proc",
            "/sys",
            "/run",
            "/var/lib",
            "/System",
            "/Library",
            "/Applications",
            "/private/etc",
            "/private/var/db",
            "/Library/Developer",
        ] {
            grants.push((PathBuf::from(system), ro));
        }
        grants.push((PathBuf::from("/dev"), rw));
    }
    #[cfg(windows)]
    windows_grants(&mut grants);
    if let Some(home) = dirs::home_dir() {
        for toolchain in [
            ".cargo",
            ".rustup",
            ".nvm",
            ".volta",
            ".bun",
            ".local",
            ".npm-global",
            ".deno",
            ".pyenv",
            ".sdkman",
            "go",
            ".linuxbrew",
            ".fnm",
            ".asdf",
            ".gitconfig",
            ".config/git",
            ".gitignore_global",
            "Library/Developer",
            "Library/Caches/Homebrew",
        ] {
            grants.push((home.join(toolchain), ro));
        }
        for cache in [
            ".cargo/registry",
            ".cargo/git",
            ".cache",
            ".npm",
            ".bun/install/cache",
            "Library/Caches",
        ] {
            grants.push((home.join(cache), rw));
        }
        let own: &[&str] = match agent {
            crate::agent::Agent::Claude => &[".claude", ".claude.json", ".config/claude"],
            crate::agent::Agent::Codex => &[".codex"],
            crate::agent::Agent::OpenCode => &[
                ".local/share/opencode",
                ".local/state/opencode",
                ".config/opencode",
                ".cache/opencode",
                ".opencode",
            ],
        };
        for folder in own {
            grants.push((home.join(folder), rw));
        }
    }
    // The harness's installation, wherever it is.
    if let Some(program) = crate::programs::find(agent.command())
        && let Ok(resolved) = dunce::canonicalize(&program)
        && let Some(folder) = resolved.parent()
    {
        grants.push((folder.to_path_buf(), ro));
    }
    // The application itself, for the piton shim.
    if let Ok(exe) = std::env::current_exe() {
        grants.push((exe, ro));
    }
    grants
}

/// What a run needs on Windows besides, as the SandboxScope's programs say:
/// Node.js, git, MSVC and the Windows SDKs, and PowerShell's modules read,
/// and the user's caches written. `C:\Windows` and `C:\Program Files` are
/// already readable by every AppContainer and need nothing more.
#[cfg(windows)]
fn windows_grants(grants: &mut Vec<(PathBuf, Access)>) {
    let ro = Access::ReadOnly;
    let rw = Access::ReadWrite;
    for program in ["node", "git", "pwsh", "cl", "link", "cargo", "rustc"] {
        if let Some(found) = crate::programs::find(program)
            && let Ok(resolved) = dunce::canonicalize(&found)
            && let Some(folder) = resolved.parent()
        {
            grants.push((folder.to_path_buf(), ro));
        }
    }
    if let Some(home) = dirs::home_dir() {
        for modules in [
            "Documents\\PowerShell\\Modules",
            "Documents\\WindowsPowerShell\\Modules",
        ] {
            grants.push((home.join(modules), ro));
        }
    }
    if let Some(roaming) = dirs::data_dir() {
        grants.push((roaming.join("npm"), ro));
    }
    if let Some(local) = dirs::data_local_dir() {
        for cache in [
            "npm-cache",
            "pip\\cache",
            "node-gyp",
            "Microsoft\\VisualStudio",
        ] {
            grants.push((local.join(cache), rw));
        }
    }
}

/// A run's sandbox, made before it starts and removed after it: its own
/// temporary directory, its filtered git repository, its piton channel, and
/// its compiled policy.
pub struct Prepared {
    pub kind: ProfileKind,
    pub compiled: Compiled,
    /// The run's own temporary directory, removed with it.
    pub temp: PathBuf,
    /// What the run is given in its environment: its temporary directory,
    /// its filtered repository, and its piton shim first on its PATH.
    pub env: Vec<(String, std::ffi::OsString)>,
    _channel: Option<channel::Server>,
}

impl Drop for Prepared {
    fn drop(&mut self) {
        // Whatever the launcher left granted, as when it was ended early.
        #[cfg(windows)]
        windows::cleanup(&self.compiled.label);
        std::fs::remove_dir_all(&self.temp).ok();
    }
}

/// Counts the runs prepared, for their folders' names.
static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Makes the sandbox for a run of `kind` by `agent` in the project at
/// `project_dir`, as the SandboxScope says, or refuses, so the run never
/// starts. Blocking.
pub fn prepare(
    kind: ProfileKind,
    agent: crate::agent::Agent,
    project_dir: &Path,
    files: &RunFiles,
) -> Result<Prepared, Refused> {
    // The platform can enforce one at all, or nothing runs.
    status()?;
    let run = format!(
        "{}-{}",
        std::process::id(),
        RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    let label = format!("Suspense.{}.{run}", kind.label());
    let temp = std::env::temp_dir().join(format!("suspense-run-{run}"));
    std::fs::remove_dir_all(&temp).ok();
    std::fs::create_dir_all(temp.join("tmp")).map_err(|err| Refused {
        message: "its temporary directory couldn't be made".into(),
        details: err.to_string(),
    })?;
    let fail = |refusal: Refused| {
        std::fs::remove_dir_all(&temp).ok();
        refusal
    };
    let mut policy = profile(kind, agent, project_dir, files).map_err(fail)?;
    policy = policy.read_write(&temp);
    let mut compiled = compile(&policy).map_err(fail)?;
    compiled.label = label;
    let scratch = temp.join("tmp");
    let mut env: Vec<(String, std::ffi::OsString)> = ["TMPDIR", "TMP", "TEMP"]
        .into_iter()
        .map(|name| (name.to_string(), scratch.clone().into_os_string()))
        .collect();
    // zsh makes its heredocs' files at TMPPREFIX, a prefix rather than a
    // folder, never at TMPDIR.
    env.push(("TMPPREFIX".into(), scratch.join("zsh").into_os_string()));
    // Git, filtered to what it may read, never the project's own.
    if kind.filtered_git() {
        match git::filtered(project_dir, &compiled, &temp.join("git")) {
            Ok(Some(git_dir)) => {
                env.push(("GIT_DIR".into(), git_dir.into_os_string()));
                env.push((
                    "GIT_WORK_TREE".into(),
                    compiled.project.clone().into_os_string(),
                ));
            }
            Ok(None) => {}
            Err(err) => {
                return Err(fail(Refused {
                    message: "its filtered git repository couldn't be made".into(),
                    details: format!("{err:#}"),
                }));
            }
        }
    }
    // Piton, through the application, as trusted work.
    let server = if kind == ProfileKind::Bare {
        None
    } else {
        match channel::Server::start(kind, project_dir, &temp, &compiled.label) {
            Ok((server, bin)) => {
                let path = std::env::var_os("PATH").unwrap_or_default();
                let mut paths = vec![bin];
                paths.extend(std::env::split_paths(&path));
                if let Ok(joined) = std::env::join_paths(paths) {
                    env.push(("PATH".into(), joined));
                }
                env.push((channel::SOCKET_ENV.into(), server.address().into()));
                Some(server)
            }
            Err(err) => {
                return Err(fail(Refused {
                    message: "its piton channel couldn't be made".into(),
                    details: format!("{err:#}"),
                }));
            }
        }
    };
    Ok(Prepared {
        kind,
        compiled,
        temp,
        env,
        _channel: server,
    })
}

impl Prepared {
    /// Puts the sandbox on `command`, the run's: its environment, and the
    /// restriction itself, in force before its program runs; or refuses,
    /// so it never runs.
    pub fn apply(&self, command: &mut Command) -> Result<(), Refused> {
        command.current_dir(&self.compiled.project);
        for (name, value) in &self.env {
            command.env(name, value);
        }
        enforce(command, &self.compiled)
    }

    /// A summary of its grants, for the debug log.
    pub fn describe(&self) -> String {
        let count = |access| {
            self.compiled
                .rules
                .iter()
                .filter(|rule| rule.access == access)
                .count()
        };
        format!(
            "{} profile: {} read-only, {} read-write, {} denied",
            self.kind.label(),
            count(Access::ReadOnly),
            count(Access::ReadWrite),
            self.compiled.denied.len()
        )
    }
}

/// Puts `compiled` on `command` with the platform's backend.
pub fn enforce(command: &mut Command, compiled: &Compiled) -> Result<(), Refused> {
    #[cfg(target_os = "linux")]
    return linux::enforce(command, compiled);
    #[cfg(target_os = "macos")]
    return macos::enforce(command, compiled);
    #[cfg(windows)]
    return windows::enforce(command, compiled);
    #[allow(unreachable_code)]
    {
        let _ = (command, compiled);
        Err(refused("this platform has no sandbox backend"))
    }
}

/// Runs a short set of the hostile checks the sandbox is tested with, in a
/// temporary project of its own, as the settings' Test sandbox does: each
/// check, and whether the system refused it. Blocking.
pub fn self_test() -> Result<Vec<(String, bool)>, Refused> {
    status()?;
    let dir = std::env::temp_dir().join(format!("suspense-sandbox-test-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let made = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dir.join("src"))?;
        std::fs::create_dir_all(dir.join("spec"))?;
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n")?;
        std::fs::write(dir.join("spec/index.pi"), "export secret: 1\n")?;
        Classification {
            code_roots: vec!["src".into()],
            spec_roots: vec!["spec".into()],
            ..Classification::default()
        }
        .save(&dir)
    })();
    made.map_err(|err| Refused {
        message: "its temporary project couldn't be made".into(),
        details: err.to_string(),
    })?;
    let git = |args: &[&str]| {
        crate::process::command("git")
            // Never a repository the application was started under.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args([
                "-c",
                "user.name=Suspense",
                "-c",
                "user.email=suspense@localhost",
            ])
            .args(args)
            .current_dir(&dir)
            .output()
            .ok();
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "test"]);
    let agent = crate::agent::Agent::Claude;
    // Each check as `sh -c` runs it, and as `cmd /d /c` runs it on Windows,
    // which has no `sh`: a check that can't start would pass as refused.
    let checks: [(ProfileKind, &str, &str, &str); 12] = [
        (
            ProfileKind::Code,
            "A Code run reads the spec",
            "cat spec/index.pi",
            r"type spec\index.pi",
        ),
        (
            ProfileKind::Code,
            "A Code run writes the spec",
            "echo x > spec/new.pi",
            r"echo x> spec\new.pi",
        ),
        (
            ProfileKind::Code,
            "A Code run reads the spec through `..`",
            "cat src/../spec/index.pi",
            r"type src\..\spec\index.pi",
        ),
        (
            ProfileKind::Code,
            "A Code run's child of a child reads the spec",
            "sh -c 'sh -c \"cat spec/index.pi\"'",
            r"cmd /d /c cmd /d /c type spec\index.pi",
        ),
        (
            ProfileKind::Code,
            "A Code run shows the spec through git",
            "git show HEAD:spec/index.pi",
            r"git show HEAD:spec/index.pi",
        ),
        (
            ProfileKind::Code,
            "A Code run moves the spec into the code",
            "mv spec/index.pi src/stolen.pi",
            r"move spec\index.pi src\stolen.pi",
        ),
        (
            ProfileKind::Code,
            "A Code run reads the spec with Python",
            "python3 -c 'open(\"spec/index.pi\").read()'",
            r"python -c open('spec/index.pi').read()",
        ),
        (
            ProfileKind::Code,
            if cfg!(windows) {
                "A Code run reads the spec through a \\\\?\\ path"
            } else {
                "A Code run reads the spec through /proc"
            },
            "cat /proc/self/cwd/spec/index.pi",
            r"type \\?\%CD%\spec\index.pi",
        ),
        (
            ProfileKind::Code,
            "A Code run hard links the spec into the code",
            "ln spec/index.pi src/linked.pi",
            r"mklink /H src\linked.pi spec\index.pi",
        ),
        (
            ProfileKind::Spec,
            "A Spec run reads the code",
            "cat src/main.rs",
            r"type src\main.rs",
        ),
        (
            ProfileKind::Spec,
            "A Spec run writes the code",
            "echo x > src/new.rs",
            r"echo x> src\new.rs",
        ),
        (
            ProfileKind::Spec,
            "A Spec run moves the code into the spec",
            "mv src/main.rs spec/main.rs",
            r"move src\main.rs spec\main.rs",
        ),
    ];
    let mut results = Vec::new();
    for (kind, label, unix, windows) in checks {
        let prepared = prepare(kind, agent, &dir, &RunFiles::default());
        let refused = match prepared {
            Ok(prepared) => {
                let mut command = if cfg!(windows) {
                    let mut command = crate::process::command("cmd.exe");
                    command.args(["/d", "/c", windows]);
                    command
                } else {
                    let mut command = crate::process::command("sh");
                    command.args(["-c", unix]);
                    command
                };
                command.stdin(std::process::Stdio::null());
                match prepared.apply(&mut command) {
                    Ok(()) => !command.output().is_ok_and(|output| output.status.success()),
                    Err(_) => true,
                }
            }
            Err(_) => true,
        };
        results.push((label.to_string(), refused));
    }
    std::fs::remove_dir_all(&dir).ok();
    Ok(results)
}

/// Runs one command under a profile, as the SandboxScope's examples do:
/// `suspense --sandbox code|spec|question|freeform <project> -- <command>...`.
/// Gives its exit status.
pub fn example(args: &[String]) -> i32 {
    let usage = || {
        eprintln!(
            "usage: suspense --sandbox code|spec|question|freeform <project> -- <command>..."
        );
        2
    };
    let (Some(kind), Some(project)) = (args.first(), args.get(1)) else {
        return usage();
    };
    let kind = match kind.as_str() {
        "code" => ProfileKind::Code,
        "spec" => ProfileKind::Spec,
        "question" => ProfileKind::Question,
        "freeform" => ProfileKind::Freeform,
        _ => return usage(),
    };
    let rest: Vec<&String> = args[2..].iter().skip_while(|arg| *arg == "--").collect();
    let Some((program, program_args)) = rest.split_first() else {
        return usage();
    };
    let project = Path::new(project);
    let agent = crate::agent::of_project(Some(project));
    let prepared = match prepare(kind, agent, project, &RunFiles::default()) {
        Ok(prepared) => prepared,
        Err(refused) => {
            eprintln!("{refused}");
            return 1;
        }
    };
    eprintln!("{}", prepared.describe());
    let mut command = Command::new(program);
    command.args(program_args.iter().map(|arg| arg.as_str()));
    if let Err(refused) = prepared.apply(&mut command) {
        eprintln!("{refused}");
        return 1;
    }
    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(err) => {
            eprintln!("{program}: {err}");
            127
        }
    }
}

#[cfg(test)]
mod tests;
