//! The prompt queue on disk. Prompts sent while the harness is working wait in
//! the project's `.suspense/queue`, each saved as its hidden anchor's source
//! the way prompt history is, and named so they list in the order they were
//! queued. A queued prompt's file goes when it is sent or cancelled.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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

/// A mark of when a prompt is queued, later than every mark before it, so
/// prompts queued one straight after another keep their order however long
/// each then takes to save.
pub fn stamp() -> u128 {
    static LAST: Mutex<u128> = Mutex::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut last = LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    *last = now.max(*last + 1);
    *last
}

/// Saves `text`, as `anchor`'s source, to the end of the project's queue.
#[cfg(test)]
pub fn add(anchor: HiddenAnchor, text: String, project_dir: &Path) -> Result<QueuedPrompt> {
    add_at(stamp(), anchor, text, project_dir)
}

/// Saves `text`, as `anchor`'s source, in the project's queue at the place
/// `queued_at`, a [`stamp`] taken when it was queued.
pub fn add_at(
    queued_at: u128,
    anchor: HiddenAnchor,
    text: String,
    project_dir: &Path,
) -> Result<QueuedPrompt> {
    let dir = queue_dir(project_dir);
    fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    // Zero-padded, so names sort in the order they were queued.
    let file = dir.join(format!("{queued_at:020}-{}.pi", anchor.name()));
    fs::write(&file, anchor.source(&text))
        .with_context(|| format!("could not save {}", file.display()))?;
    Ok(QueuedPrompt { file, anchor, text })
}

/// Saves `text`, as `anchor`'s source, in place of the queued prompt in
/// `file`, keeping its place in the queue.
pub fn replace(file: PathBuf, anchor: HiddenAnchor, text: String) -> Result<QueuedPrompt> {
    fs::write(&file, anchor.source(&text))
        .with_context(|| format!("could not save {}", file.display()))?;
    Ok(QueuedPrompt { file, anchor, text })
}

/// Saves `queued` again as it now is, as when it is marked to start a new
/// conversation, keeping its place in the queue.
pub fn rewrite(queued: &QueuedPrompt) -> Result<()> {
    fs::write(&queued.file, queued.anchor.source(&queued.text))
        .with_context(|| format!("could not save {}", queued.file.display()))
}

/// Swaps the places of two queued prompts in the queue: each takes the other's
/// mark of when it was queued, which orders the files, keeping its own name.
pub fn swap(a: &mut QueuedPrompt, b: &mut QueuedPrompt) -> Result<()> {
    let renamed = |from: &QueuedPrompt, to: &QueuedPrompt| {
        to.file.with_file_name(format!(
            "{}-{}.pi",
            stamp_of(&to.file).unwrap_or_default(),
            from.anchor.name()
        ))
    };
    let (new_a, new_b) = (renamed(a, b), renamed(b, a));
    let parked = a.file.with_extension("moving");
    fs::rename(&a.file, &parked).with_context(|| format!("could not move {}", a.file.display()))?;
    if let Err(err) = fs::rename(&b.file, &new_b) {
        fs::rename(&parked, &a.file).ok();
        return Err(err).with_context(|| format!("could not move {}", b.file.display()));
    }
    if let Err(err) = fs::rename(&parked, &new_a) {
        fs::rename(&new_b, &b.file).ok();
        fs::rename(&parked, &a.file).ok();
        return Err(err).with_context(|| format!("could not move {}", a.file.display()));
    }
    a.file = new_a;
    b.file = new_b;
    Ok(())
}

/// The mark of when the prompt in `file` was queued, as its name starts.
fn stamp_of(file: &Path) -> Option<&str> {
    file.file_name()?
        .to_str()?
        .split_once('-')
        .map(|(stamp, _)| stamp)
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

    use super::{add, add_at, load, remove, replace, stamp, swap};
    use crate::hidden_anchor::HiddenAnchor;

    /// Prompts queued in a row keep the order they were queued in, whatever
    /// order they are then saved in, as when a batch is sent to the other
    /// mode and each compiles in the background.
    #[test]
    fn queued_prompts_keep_the_order_they_were_queued_in() {
        let project_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/prompt-queue-order-test");
        fs::remove_dir_all(&project_dir).ok();
        let stamps: Vec<u128> = (0..3).map(|_| stamp()).collect();
        assert!(
            stamps.windows(2).all(|pair| pair[0] < pair[1]),
            "{stamps:?}"
        );
        for (n, queued_at) in stamps.into_iter().enumerate().rev() {
            add_at(
                queued_at,
                HiddenAnchor::random(),
                format!("{n}"),
                &project_dir,
            )
            .unwrap();
        }
        let texts: Vec<String> = load(&project_dir).into_iter().map(|q| q.text).collect();
        assert_eq!(texts, ["0", "1", "2"]);
        fs::remove_dir_all(&project_dir).ok();
    }

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

        // Edited, a prompt keeps its place.
        let second = load(&project_dir).remove(1);
        replace(second.file, HiddenAnchor::random(), "second, edited".into()).unwrap();
        let texts: Vec<String> = load(&project_dir)
            .into_iter()
            .map(|queued| queued.text)
            .collect();
        assert_eq!(texts[1], "second, edited");

        // Swapped, two prompts trade places, and load back so.
        let mut queue = load(&project_dir);
        let (head, tail) = queue.split_at_mut(1);
        swap(&mut head[0], &mut tail[0]).unwrap();
        let texts: Vec<String> = load(&project_dir)
            .into_iter()
            .map(|queued| queued.text)
            .collect();
        assert_eq!(
            texts[..2],
            ["second, edited", "Update @{ApplicationScope}\n\n  indented"]
        );
        let (head, tail) = queue.split_at_mut(1);
        swap(&mut head[0], &mut tail[0]).unwrap();
        assert_eq!(queue[0].file, first.file);

        remove(&first.file).unwrap();
        remove(&first.file).unwrap();
        let texts: Vec<String> = load(&project_dir)
            .into_iter()
            .map(|queued| queued.text)
            .collect();
        assert_eq!(texts, ["second, edited", ""]);
    }
}
