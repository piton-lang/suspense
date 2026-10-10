//! Files attached to a prompt, other than the images the harness is given as
//! images: pasted, dropped onto the chat input, or chosen with its Attach
//! files button, up to 25 MB each. Once the prompt is sent or queued, each
//! is saved in the project's data, under `attachments/`, under its own name
//! in a folder named by the second it was saved and a short hash of its
//! content, so the same file attached twice is saved once; the hidden anchor
//! keeps their paths, and the harness is told where they are, never given
//! what they hold.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
use gpui_kit::*;
use sha2::{Digest as _, Sha256};

use crate::attached_image::{self, AttachedImage};
use crate::file_link::OpenFile;
use crate::hidden_anchor::APP_DIR;

/// The largest file that can be attached, in bytes.
pub const MAX_BYTES: u64 = 25 * 1024 * 1024;

/// Where the project's attached files are saved, in its data.
const ATTACHMENTS_DIR: &str = "attachments";

/// How many hex digits of a file's hash its folder's name holds.
const HASH_DIGITS: usize = 12;

/// A file attached to the prompt being written, held as it was when
/// attached until it is sent.
#[derive(Clone, Debug, PartialEq)]
pub struct AttachedFile {
    /// Its name, as it is saved.
    pub name: String,
    /// What it holds.
    pub bytes: Arc<Vec<u8>>,
}

/// Something attached from a file: an image the harness is given as one, or
/// any other file.
#[derive(Clone, Debug, PartialEq)]
pub enum Loaded {
    Image(AttachedImage),
    File(AttachedFile),
}

/// Why a file wasn't attached, as a notification says it.
#[derive(Clone, Debug, PartialEq)]
pub enum Rejected {
    /// It is a folder.
    Folder(String),
    /// It is larger than [`MAX_BYTES`].
    TooLarge(String),
    /// It couldn't be read.
    Unreadable(String, String),
}

impl Rejected {
    /// What the notification says.
    pub fn message(&self) -> String {
        match self {
            Self::Folder(name) => format!("{name} is a folder: folders can't be attached."),
            Self::TooLarge(name) => {
                format!("{name} is too large: files can be attached up to 25 MB.")
            }
            Self::Unreadable(name, error) => format!("{name} couldn't be read: {error}"),
        }
    }
}

/// The file at `path`, dropped, chosen, or copied, as an attachment: an image
/// where its content is PNG, JPEG, GIF, or WebP of up to 5 MB, else a file
/// of up to 25 MB. Its size is checked before it is read, so a large file is
/// never read whole.
pub fn load(path: &Path) -> Result<Loaded, Rejected> {
    let name = name_of(path);
    let metadata =
        fs::metadata(path).map_err(|err| Rejected::Unreadable(name.clone(), err.to_string()))?;
    if metadata.is_dir() {
        return Err(Rejected::Folder(name));
    }
    if metadata.len() > MAX_BYTES {
        return Err(Rejected::TooLarge(name));
    }
    let bytes =
        fs::read(path).map_err(|err| Rejected::Unreadable(name.clone(), err.to_string()))?;
    Ok(from_bytes(bytes, name))
}

/// `bytes`, a file named `name`, as an attachment, as [`load`] gives it.
pub fn from_bytes(bytes: Vec<u8>, name: String) -> Loaded {
    if bytes.len() as u64 <= attached_image::MAX_BYTES && attached_image::format_of(&bytes).is_some()
    {
        if let Ok(image) = AttachedImage::from_bytes(bytes.clone(), Some(name.clone())) {
            return Loaded::Image(image);
        }
    }
    Loaded::File(AttachedFile {
        name,
        bytes: Arc::new(bytes),
    })
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl AttachedFile {
    /// Its size, as its row shows it.
    pub fn size_label(&self) -> String {
        size_label(self.bytes.len() as u64)
    }

    /// Saves it in the project's data, unless the same file is saved there
    /// already under the same name, and gives its path from the project
    /// directory.
    pub fn save(&self, project_dir: &Path) -> Result<String> {
        let dir = attachments_dir(project_dir);
        fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
        let hash = Sha256::digest(self.bytes.as_slice())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let suffix = format!("-{}", &hash[..HASH_DIGITS]);
        let existing = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .find(|folder| folder.ends_with(&suffix) && dir.join(folder).join(&self.name).is_file());
        let folder = match existing {
            Some(folder) => folder,
            None => {
                let saved_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_secs());
                let folder = format!("{saved_at}{suffix}");
                let file = dir.join(&folder).join(&self.name);
                fs::create_dir_all(dir.join(&folder))
                    .with_context(|| format!("could not create {}", dir.join(&folder).display()))?;
                fs::write(&file, self.bytes.as_slice())
                    .with_context(|| format!("could not save {}", file.display()))?;
                folder
            }
        };
        Ok(format!("{APP_DIR}/{ATTACHMENTS_DIR}/{folder}/{}", self.name))
    }
}

/// A size in bytes as a file's row shows it: "14 KB" or "2.3 MB".
pub fn size_label(bytes: u64) -> String {
    const KB: f64 = 1024.;
    const MB: f64 = KB * 1024.;
    let bytes = bytes as f64;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{} KB", (bytes / KB).round().max(1.))
    } else if bytes < 10. * MB {
        format!("{:.1} MB", bytes / MB)
    } else {
        format!("{} MB", (bytes / MB).round())
    }
}

/// The project's attached files.
pub fn attachments_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(APP_DIR).join(ATTACHMENTS_DIR)
}

/// Saves each of `files` as [`AttachedFile::save`] does, in order.
pub fn save_all(files: &[AttachedFile], project_dir: &Path) -> Result<Vec<String>> {
    files.iter().map(|file| file.save(project_dir)).collect()
}

/// The saved files at `paths`, from the project directory, read back to be
/// attached again, as when a queued prompt is edited; any no longer there is
/// left out.
pub fn load_saved(paths: &[String], project_dir: Option<&Path>) -> Vec<AttachedFile> {
    paths
        .iter()
        .filter_map(|path| {
            let file = attached_image::resolve(path, project_dir);
            let bytes = fs::read(&file).ok()?;
            Some(AttachedFile {
                name: name_of(&file),
                bytes: Arc::new(bytes),
            })
        })
        .collect()
}

/// The files at `paths`, from `project_dir`, as files on the host.
pub fn files(paths: &[String], project_dir: &Path) -> Vec<PathBuf> {
    paths.iter().map(|path| project_dir.join(path)).collect()
}

/// What the harness is told of the files saved at `paths`, after the prompt
/// and any attached text: "Attached files:", then each one's path from the
/// project directory, its original name, and its size, a line each. Nothing
/// without any.
pub fn harness_lines(paths: &[String], project_dir: Option<&Path>) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let mut lines = String::from("\n\nAttached files:");
    for path in paths {
        let file = attached_image::resolve(path, project_dir);
        let size = fs::metadata(&file).map(|meta| size_label(meta.len()));
        lines.push_str(&format!("\n- {path} ({}", name_of(&file)));
        if let Ok(size) = size {
            lines.push_str(&format!(", {size}"));
        }
        lines.push(')');
    }
    lines
}

/// The first of `files` that isn't there, as a run that can't start says.
pub fn missing(files: &[PathBuf]) -> Option<String> {
    files.iter().find(|file| !file.is_file()).map(|file| {
        format!("The attached file {} couldn't be found.", file.display())
    })
}

/// The kinds of file a row tells apart by its icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Document,
    Code,
    Other,
}

const DOCUMENTS: [&str; 14] = [
    "txt", "md", "markdown", "pdf", "doc", "docx", "odt", "rtf", "csv", "tsv", "xls", "xlsx",
    "ppt", "pptx",
];

const CODE: [&str; 34] = [
    "rs", "pi", "py", "js", "jsx", "ts", "tsx", "go", "c", "h", "cpp", "hpp", "cc", "cs", "java",
    "kt", "swift", "rb", "php", "sh", "bash", "zsh", "lua", "zig", "scala", "sql", "html", "css",
    "scss", "json", "toml", "yaml", "yml", "xml",
];

/// The kind of the file `name`, by its extension.
pub fn kind_of(name: &str) -> Kind {
    let ext = Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if DOCUMENTS.contains(&ext.as_str()) {
        Kind::Document
    } else if CODE.contains(&ext.as_str()) {
        Kind::Code
    } else {
        Kind::Other
    }
}

/// The icon a row shows for the file `name`.
pub fn icon(name: &str) -> IconName {
    match kind_of(name) {
        Kind::Document => IconName::FileText,
        Kind::Code => IconName::FileCode,
        Kind::Other => IconName::File,
    }
}

/// Whether the file at `path` can be shown in the editor: text, as far as
/// its start shows.
fn is_text(path: &Path) -> bool {
    use std::io::Read as _;
    let mut start = vec![0; 8192];
    let Ok(read) = fs::File::open(path).and_then(|mut file| file.read(&mut start)) else {
        return false;
    };
    start.truncate(read);
    // Valid UTF-8 without NULs, but for a character cut off at its end.
    !start.contains(&0)
        && std::str::from_utf8(&start).map_or_else(|err| err.error_len().is_none(), |_| true)
}

/// Opens the file at `path`: in the editor, by `open`, where it can be
/// shown there, or else in the platform's default application for it.
pub fn open(path: PathBuf, open: Option<&OpenFile>, window: &mut Window, cx: &mut App) {
    match open {
        Some(open) if is_text(&path) => open(path, window, cx),
        _ => cx.open_with_system(&path),
    }
}

/// The files saved at `paths`, from the project directory, listed beneath a
/// prompt, each as its row in the chat input shows it, without a remove
/// button, opening when clicked; none when there are none.
pub fn prompt_files(
    id: impl Into<ElementId>,
    paths: &[String],
    project_dir: Option<&Path>,
    opener: Option<OpenFile>,
    cx: &App,
) -> Option<AnyElement> {
    if paths.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let (border, radius, muted) = (theme.border, theme.radius, theme.muted_foreground);
    let rows = paths.iter().enumerate().map(|(ix, path)| {
        let file = attached_image::resolve(path, project_dir);
        let name = name_of(&file);
        let size = fs::metadata(&file)
            .map(|meta| size_label(meta.len()))
            .unwrap_or_default();
        let opener = opener.clone();
        let row = h_flex()
            .id(("prompt-file", ix))
            .gap_2()
            .px_2()
            .py_1()
            .rounded(radius)
            .border_1()
            .border_color(border)
            .text_sm()
            .cursor_pointer()
            .child(Icon::new(icon(&name)).small().text_color(muted))
            .child(div().flex_1().min_w_0().truncate().child(name))
            .child(div().flex_none().text_xs().text_color(muted).child(size))
            .on_click(move |_, window, cx| open(file.clone(), opener.as_ref(), window, cx));
        // Lets UI tests find the row; inert in normal builds.
        gpui_kit::TestSupportExt::test_support(row)
    });
    let list = div().id(id).flex().flex_col().gap_1().children(rows);
    // Lets UI tests find the list; inert in normal builds.
    Some(gpui_kit::TestSupportExt::test_support(list).into_any_element())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use super::{
        AttachedFile, Kind, Loaded, MAX_BYTES, Rejected, attachments_dir, files, harness_lines,
        kind_of, load, load_saved, missing, name_of, save_all, size_label,
    };
    use crate::attached_image;
    use crate::attached_image::tests::{png, scratch};

    /// Images in the formats the harness takes, up to 5 MB, are attached as
    /// images; anything else, up to 25 MB, as a file; folders and larger
    /// files are not attached.
    #[test]
    fn files_are_attached_as_images_or_files() {
        let dir = scratch("files");
        let shot = dir.join("shot.png");
        fs::write(&shot, png(4, 4, "a")).unwrap();
        assert!(matches!(load(&shot), Ok(Loaded::Image(_))));
        let notes = dir.join("notes.md");
        fs::write(&notes, "# Notes").unwrap();
        let Ok(Loaded::File(file)) = load(&notes) else {
            panic!("not a file");
        };
        assert_eq!(file.name, "notes.md");
        // An image too large to give as one is a file.
        let large = dir.join("large.png");
        let mut bytes = png(4, 4, "b");
        bytes.resize(attached_image::MAX_BYTES as usize + 1, 0);
        fs::write(&large, &bytes).unwrap();
        assert!(matches!(load(&large), Ok(Loaded::File(_))));
        assert_eq!(load(&dir), Err(Rejected::Folder(name_of(&dir))));
        bytes.resize(MAX_BYTES as usize + 1, 0);
        fs::write(&large, &bytes).unwrap();
        assert_eq!(load(&large), Err(Rejected::TooLarge("large.png".into())));
        assert!(matches!(load(&dir.join("gone")), Err(Rejected::Unreadable(..))));
    }

    /// Saved files keep their name, in a folder named by the second and a
    /// short hash; the same file is saved once, and the harness is told
    /// where each is, its name, and its size.
    #[test]
    fn the_same_file_is_saved_once() {
        let dir = scratch("save-files");
        let file = |name: &str, text: &str| AttachedFile {
            name: name.into(),
            bytes: Arc::new(text.as_bytes().to_vec()),
        };
        let saved = save_all(
            &[file("a.txt", "one"), file("a.txt", "one"), file("a.txt", "two")],
            &dir,
        )
        .unwrap();
        assert_eq!(saved[0], saved[1]);
        assert_ne!(saved[0], saved[2]);
        assert!(saved[0].starts_with(".suspense/attachments/") && saved[0].ends_with("/a.txt"));
        assert_eq!(fs::read_dir(attachments_dir(&dir)).unwrap().count(), 2);
        assert_eq!(load_saved(&saved, Some(&dir))[2].bytes.as_slice(), b"two");
        let lines = harness_lines(&saved[..1], Some(&dir));
        assert_eq!(lines, format!("\n\nAttached files:\n- {} (a.txt, 3 B)", saved[0]));
        assert_eq!(harness_lines(&[], Some(&dir)), "");
        assert!(missing(&files(&saved, &dir)).is_none());
        assert!(missing(&[dir.join("gone")]).is_some());
    }

    #[test]
    fn sizes_and_kinds_read_as_rows_show_them() {
        assert_eq!(size_label(14 * 1024), "14 KB");
        assert_eq!(size_label(2_411_724), "2.3 MB");
        assert_eq!(kind_of("a.pdf"), Kind::Document);
        assert_eq!(kind_of("main.rs"), Kind::Code);
        assert_eq!(kind_of("archive.zip"), Kind::Other);
    }
}
