//! Links and paths in prompts and harness output that name a file, and
//! opening them in the editor.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;

use crate::project_directory::ProjectDirectory;

/// Opens a file in the editor.
pub type OpenFile = Arc<dyn Fn(PathBuf, &mut Window, &mut App) + Send + Sync>;

/// The file `target` names, if it exists: a path, or a `file://` URL, that is
/// absolute or relative to the project directory. A line or column suffix
/// (`a.rs:12:3`) and a `#fragment` are left off. A relative link written from
/// some other file (`../lsp/LSP.md`) is found as the one project file whose
/// path ends with what is left once its leading `./` and `../` are dropped.
pub fn resolve(target: &str, project_dir: Option<&Path>) -> Option<PathBuf> {
    let target = target.trim();
    let target = target.strip_prefix("file://").unwrap_or(target);
    if target.contains("://") || target.starts_with("mailto:") {
        return None;
    }
    let target = target.split('#').next().unwrap_or_default();
    if target.is_empty() {
        return None;
    }

    let candidates = std::iter::successors(Some(target), |path| {
        let (rest, suffix) = path.rsplit_once(':')?;
        (!suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())).then_some(rest)
    });
    for candidate in candidates {
        let path = Path::new(candidate);
        if path.is_absolute() {
            if path.is_file() {
                return Some(path.to_path_buf());
            }
            continue;
        }
        let Some(dir) = project_dir else { continue };
        if dir.join(path).is_file() {
            return Some(dir.join(path));
        }
        if let Some(found) = find_by_suffix(path, dir) {
            return Some(found);
        }
    }
    None
}

/// The project file whose path ends with `path` less its leading `./` and
/// `../`, when exactly one does. Ignored files are left out.
fn find_by_suffix(path: &Path, dir: &Path) -> Option<PathBuf> {
    let suffix: PathBuf = path
        .components()
        .skip_while(|component| matches!(component, Component::CurDir | Component::ParentDir))
        .collect();
    if suffix.as_os_str().is_empty() || suffix == path {
        return None;
    }
    let mut matches = ignore::WalkBuilder::new(dir)
        .hidden(false)
        .build()
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|found| found.ends_with(&suffix));
    let found = matches.next()?;
    matches.next().is_none().then_some(found)
}

/// Opens the file a clicked link names in the editor; a web link opens in the
/// browser, and a link to a file that is not there says so.
pub fn open_link(url: &str, open: &OpenFile, window: &mut Window, cx: &mut App) {
    match resolve(url, ProjectDirectory::get(cx).as_deref()) {
        Some(path) => open(path, window, cx),
        None if url.contains("://") || url.starts_with("mailto:") => cx.open_url(url),
        None => window.push_notification(Notification::error(format!("No file at {url}.")), cx),
    }
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn links_resolve_to_project_files() {
        let dir = std::env::temp_dir().join(format!("suspense-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let scope = dir.join(".claude/reference/scope");
        std::fs::create_dir_all(scope.join("lsp")).unwrap();
        std::fs::create_dir_all(scope.join("editor")).unwrap();
        std::fs::write(scope.join("lsp/LSP.md"), "").unwrap();
        std::fs::write(scope.join("editor/Scope.md"), "").unwrap();
        std::fs::write(scope.join("lsp/Scope.md"), "").unwrap();
        let root = Some(dir.as_path());
        let lsp = scope.join("lsp/LSP.md");

        assert_eq!(
            resolve(".claude/reference/scope/lsp/LSP.md", root),
            Some(lsp.clone())
        );
        assert_eq!(resolve(lsp.to_str().unwrap(), None), Some(lsp.clone()));
        assert_eq!(
            resolve(&format!("file://{}#concept", lsp.display()), None),
            Some(lsp.clone())
        );
        assert_eq!(
            resolve(".claude/reference/scope/lsp/LSP.md:12:3", root),
            Some(lsp.clone())
        );
        assert_eq!(resolve("../../lsp/LSP.md", root), Some(lsp));
        // Two files end with the same path: which is meant is not known.
        assert_eq!(resolve("../Scope.md", root), None);
        assert_eq!(resolve("LSP.md", root), None);
        assert_eq!(resolve("https://gpui-kit.com/", root), None);
        assert_eq!(resolve(".claude/missing.md", root), None);

        std::fs::remove_dir_all(&dir).ok();
    }
}
