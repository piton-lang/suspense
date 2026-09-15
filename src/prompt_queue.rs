//! The prompt queue on disk. Prompts sent while the harness is working wait in
//! the project's `.suspense/queue`, each saved as its hidden anchor's source
//! the way prompt history is, and named so they list in the order they were
//! queued. A queued prompt's file goes when it is sent or cancelled.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};

use crate::hidden_anchor::{APP_DIR, HiddenAnchor};

const QUEUE_DIR: &str = "queue";

/// A prompt waiting in the queue, and the file it is saved in.
pub struct QueuedPrompt {
    pub file: PathBuf,
    pub anchor: HiddenAnchor,
    pub text: String,
}

fn queue_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(QUEUE_DIR)
}

/// Saves `text`, as `anchor`'s source, to the end of the project's queue.
pub fn add(anchor: HiddenAnchor, text: String, project_dir: &Path) -> Result<QueuedPrompt> {
    let dir = queue_dir(project_dir);
    fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let queued_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    // Zero-padded, so names sort in the order they were queued.
    let file = dir.join(format!("{queued_at:020}-{}.pi", anchor.name()));
    fs::write(&file, anchor.source(&text))
        .with_context(|| format!("could not save {}", file.display()))?;
    Ok(QueuedPrompt { file, anchor, text })
}

/// The project's queued prompts, in the order they were queued. Files that do
/// not read back as a hidden anchor are left out.
pub fn load(project_dir: &Path) -> Vec<QueuedPrompt> {
    let Ok(entries) = fs::read_dir(queue_dir(project_dir)) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "pi"))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|file| {
            let (anchor, text) = HiddenAnchor::parse(&fs::read_to_string(&file).ok()?)?;
            Some(QueuedPrompt { file, anchor, text })
        })
        .collect()
}

/// Removes a queued prompt's file; one already gone is not an error.
pub fn remove(file: &Path) -> Result<()> {
    match fs::remove_file(file) {
        Err(err) if err.kind() != ErrorKind::NotFound => {
            Err(err).with_context(|| format!("could not remove {}", file.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{add, load, remove};
    use crate::hidden_anchor::HiddenAnchor;

    /// Queued prompts load back in the order they were queued, imports and
    /// all, and are gone once removed.
    #[test]
    fn queued_prompts_load_back_in_order() {
        let project_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/prompt-queue-test");
        fs::remove_dir_all(&project_dir).ok();

        let mut first = HiddenAnchor::random();
        first
            .imports
            .add_from_source("from ./scope/application import ApplicationScope");
        let first_name = first.name().to_string();
        let first = add(
            first,
            "Update @{ApplicationScope}\n\n  indented".into(),
            &project_dir,
        )
        .unwrap();
        add(HiddenAnchor::random(), "second".into(), &project_dir).unwrap();
        add(HiddenAnchor::random(), String::new(), &project_dir).unwrap();

        let queue = load(&project_dir);
        let texts: Vec<&str> = queue.iter().map(|queued| queued.text.as_str()).collect();
        assert_eq!(
            texts,
            ["Update @{ApplicationScope}\n\n  indented", "second", ""]
        );
        assert_eq!(queue[0].anchor.name(), first_name);
        assert_eq!(
            queue[0].anchor.source(&queue[0].text),
            fs::read_to_string(&first.file).unwrap()
        );

        remove(&first.file).unwrap();
        remove(&first.file).unwrap();
        let texts: Vec<String> = load(&project_dir)
            .into_iter()
            .map(|queued| queued.text)
            .collect();
        assert_eq!(texts, ["second", ""]);
    }
}
