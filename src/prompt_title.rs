//! Naming a task or question after what it asks for: the harness titles the
//! prompt as typed, with the prompt in `system-prompts/prompt-title.pi`, and
//! the title becomes its hidden anchor's name, such as
//! `ChatTitlesFromTheAgent`, numbered when the project already has one of
//! that name. A title that doesn't come quickly, or can't be had, leaves the
//! prompt with a random name, as [`HiddenAnchor::random_name`] gives.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::hidden_anchor::{APP_DIR, HiddenAnchor};

/// How long a prompt waits for its title before it is named at random.
const WAIT: Duration = Duration::from_secs(10);

/// The longest a name from a title is, before any number.
const MAX_CHARS: usize = 40;

/// The most of a prompt handed to the harness to title, so a title is always
/// quick to write.
#[cfg_attr(test, allow(dead_code))]
const MAX_PROMPT_CHARS: usize = 4_000;

/// Where the names a project's tasks and questions are saved under are.
const DIRS: [&str; 3] = ["history", "asks", "queue"];

/// Asks the harness for a prompt's title.
pub type Titler = fn(&Path, &str) -> Result<String>;

/// Names given out by this run of the application, by project, so two
/// prompts named at once, before either is saved, never share one.
static GIVEN: LazyLock<Mutex<HashSet<(PathBuf, String)>>> = LazyLock::new(Default::default);

/// Asks the harness for the title of `prompt`, quickly, with a small model.
#[cfg_attr(test, allow(dead_code))]
pub fn ask(project_dir: &Path, prompt: &str) -> Result<String> {
    // Written in system-prompts/prompt-title.pi.
    use crate::baked_prompts::prompt_title;
    let prompt: String = prompt.chars().take(MAX_PROMPT_CHARS).collect();
    let request = format!(
        "{}\n\n{}:\n{prompt}",
        prompt_title::REQUEST,
        prompt_title::PROMPT
    );
    crate::harness::ask_quickly(project_dir, &request)
}

/// Stands in for [`ask`] where no harness should be run, as in tests: every
/// prompt is named at random.
#[cfg(test)]
pub fn never(_: &Path, _: &str) -> Result<String> {
    anyhow::bail!("no titles in tests")
}

/// A name being found for a prompt: see [`start`].
pub struct Naming {
    project_dir: PathBuf,
    base: Option<String>,
    title: Option<(Receiver<Result<String>>, Instant)>,
}

/// Starts naming a prompt of `project_dir` that asks `prompt`. One
/// `named_after` a prompt sent before, resent or a chain's next step, is
/// named after it without asking again, unless that one's name was random.
pub fn start(
    project_dir: &Path,
    prompt: &str,
    named_after: Option<&str>,
    titler: Titler,
) -> Naming {
    let base = named_after.and_then(|name| base_of(name, project_dir));
    let title = base.is_none().then(|| {
        let (sender, receiver) = mpsc::channel();
        let (dir, prompt) = (project_dir.to_path_buf(), prompt.to_string());
        std::thread::spawn(move || sender.send(titler(&dir, &prompt)).ok());
        (receiver, Instant::now() + WAIT)
    });
    Naming {
        project_dir: project_dir.to_path_buf(),
        base,
        title,
    }
}

impl Naming {
    /// The prompt's name, waiting for its title until it has been asked for
    /// as long as it may be; random when there is none to be had.
    pub fn wait(self) -> String {
        let base = self.base.or_else(|| {
            let (receiver, deadline) = self.title?;
            let timeout = deadline.saturating_duration_since(Instant::now());
            anchor_name(&receiver.recv_timeout(timeout).ok()?.ok()?)
        });
        match base {
            Some(base) => unique(&base, &self.project_dir),
            None => HiddenAnchor::random_name(),
        }
    }
}

/// The name `title` gives an anchor: its words, each begun with a capital,
/// run together, with only letters and digits kept, cut at a whole word to
/// [`MAX_CHARS`]; begun with `Prompt` rather than a digit. `None` when no
/// letter is left.
pub fn anchor_name(title: &str) -> Option<String> {
    let line = title.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut name = String::new();
    for word in line.split(|c: char| !c.is_alphanumeric()) {
        let mut chars = word.chars();
        let Some(first) = chars.next() else {
            continue;
        };
        let word: String = first.to_uppercase().chain(chars).collect();
        if name.chars().count() + word.chars().count() > MAX_CHARS {
            // A first word too long for a name alone is cut where it must be.
            if name.is_empty() {
                name = word.chars().take(MAX_CHARS).collect();
            }
            break;
        }
        name.push_str(&word);
    }
    if !name.chars().any(char::is_alphabetic) {
        return None;
    }
    if name.starts_with(|c: char| c.is_numeric()) {
        name.insert_str(0, "Prompt");
    }
    Some(name)
}

/// Whether `name` is a random one, rather than one from a title.
fn is_random(name: &str) -> bool {
    name.starts_with("Prompt_")
}

/// The name a prompt sent after one named `name` is numbered from: `name`
/// without the number numbering it added, if any; `None` for a random name.
fn base_of(name: &str, project_dir: &Path) -> Option<String> {
    if is_random(name) || name.is_empty() {
        return None;
    }
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
    // A number is the numbering's only when the name it numbers is used, so
    // a title ending in a number keeps it.
    let numbered = base.len() < name.len()
        && !base.is_empty()
        && name[base.len()..].parse::<u64>().is_ok_and(|n| n >= 2)
        && used(project_dir).contains(base);
    Some(if numbered { base } else { name }.to_string())
}

/// The names the project's tasks and questions are saved under, in its
/// history, its questions, and its queue.
fn used(project_dir: &Path) -> HashSet<String> {
    DIRS.iter()
        .filter_map(|dir| fs::read_dir(project_dir.join(APP_DIR).join(dir)).ok())
        .flatten()
        .filter_map(|entry| {
            let file = entry.ok()?.file_name();
            let file = file.to_str()?;
            let (_, name) = file.split_once('-')?;
            Some(name.split('.').next()?.to_string())
        })
        .collect()
}

/// `base`, or, if the project already has a task or question of that name,
/// `base` with the lowest number from 2 that makes it unused.
fn unique(base: &str, project_dir: &Path) -> String {
    let used = used(project_dir);
    let mut given = GIVEN.lock().unwrap();
    let taken = |name: &str| {
        used.contains(name) || given.contains(&(project_dir.to_path_buf(), name.to_string()))
    };
    let name = std::iter::once(base.to_string())
        .chain((2..).map(|n| format!("{base}{n}")))
        .find(|name| !taken(name))
        .unwrap();
    given.insert((project_dir.to_path_buf(), name.clone()));
    name
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{anchor_name, base_of, start, unique};

    #[test]
    fn titles_become_anchor_names() {
        assert_eq!(
            anchor_name("Chat titles from the agent.").as_deref(),
            Some("ChatTitlesFromTheAgent")
        );
        assert_eq!(
            anchor_name("\"Fix scroll-lock\"\n").as_deref(),
            Some("FixScrollLock")
        );
        assert_eq!(anchor_name("404 page").as_deref(), Some("Prompt404Page"));
        assert_eq!(anchor_name("42 ... !"), None);
        assert_eq!(anchor_name(""), None);
        // Cut at a whole word.
        let long = anchor_name("Make every single panel in the right sidebar resizable").unwrap();
        assert_eq!(long, "MakeEverySinglePanelInTheRightSidebar");
        assert!(long.len() <= 40);
    }

    #[test]
    fn names_are_numbered_once_used() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/prompt-title-test");
        std::fs::remove_dir_all(&dir).ok();
        let history = dir.join(".suspense/history");
        std::fs::create_dir_all(&history).unwrap();
        std::fs::write(history.join("1-AddTheRibbon.pi"), "").unwrap();
        std::fs::write(history.join("1-AddTheRibbon.json"), "").unwrap();
        std::fs::write(history.join("2-Fix42.pi"), "").unwrap();
        assert_eq!(unique("AddTheRibbon", &dir), "AddTheRibbon2");
        // Given out already, though not yet saved.
        assert_eq!(unique("AddTheRibbon", &dir), "AddTheRibbon3");
        assert_eq!(unique("FixScrolling", &dir), "FixScrolling");

        std::fs::write(history.join("3-AddTheRibbon2.pi"), "").unwrap();
        assert_eq!(
            base_of("AddTheRibbon2", &dir).as_deref(),
            Some("AddTheRibbon")
        );
        assert_eq!(base_of("Fix42", &dir).as_deref(), Some("Fix42"));
        assert_eq!(base_of("Prompt_0123456789abcdef", &dir), None);

        // Resent, it isn't titled again.
        fn fails(_: &Path, _: &str) -> anyhow::Result<String> {
            panic!("asked for a title")
        }
        assert_eq!(start(&dir, "x", Some("Fix42"), fails).wait(), "Fix422");
        // No title, a random name.
        assert!(
            start(&dir, "x", None, super::never)
                .wait()
                .starts_with("Prompt_")
        );
        fn titled(_: &Path, _: &str) -> anyhow::Result<String> {
            Ok("Tidy the queue".into())
        }
        assert_eq!(start(&dir, "x", None, titled).wait(), "TidyTheQueue");
    }
}
