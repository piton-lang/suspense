//! The templates a new project can start from, from the `project-templates`
//! folder at the root of this repository, built into the application (see
//! `build.rs`). Each is a folder holding a template.json, giving its name,
//! description, and place in the order, and in a spec folder the files it
//! seeds the project's spec root with.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::Deserialize;

mod baked {
    include!(concat!(env!("OUT_DIR"), "/project_templates.rs"));
}

#[derive(Deserialize)]
struct Manifest {
    name: String,
    description: String,
    order: i64,
}

/// A template a project can start from.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectTemplate {
    /// Its folder's name.
    pub key: &'static str,
    pub name: String,
    pub description: String,
    order: i64,
    /// Its files, by path relative to the spec root.
    pub files: &'static [(&'static str, &'static str)],
}

/// Every template, in order.
pub fn all() -> Vec<ProjectTemplate> {
    let mut templates: Vec<ProjectTemplate> = baked::TEMPLATES
        .iter()
        .map(|(key, manifest, files)| {
            let manifest: Manifest = serde_json::from_str(manifest)
                .unwrap_or_else(|err| panic!("project-templates/{key}/template.json: {err}"));
            ProjectTemplate {
                key,
                name: manifest.name,
                description: manifest.description,
                order: manifest.order,
                files,
            }
        })
        .collect();
    templates.sort_by(|a, b| a.order.cmp(&b.order).then(a.key.cmp(b.key)));
    templates
}

impl ProjectTemplate {
    /// Writes its files into `spec_root`, leaving any already there as they
    /// are; with none, an empty index.pi.
    pub fn write(&self, spec_root: &Path) -> Result<()> {
        let files: &[(&str, &str)] = if self.files.is_empty() {
            &[("index.pi", "")]
        } else {
            self.files
        };
        for (relative, text) in files {
            let path = spec_root.join(relative);
            if path.exists() {
                continue;
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("Couldn't create {}", dir.display()))?;
            }
            std::fs::write(&path, text)
                .with_context(|| format!("Couldn't write {}", path.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /// Both templates are built in, in order, each with an index.pi.
    #[test]
    fn templates_are_built_in() {
        let templates = super::all();
        let keys: Vec<&str> = templates.iter().map(|template| template.key).collect();
        assert_eq!(keys, ["base", "scope-concept-shape"]);
        assert_eq!(templates[0].name, "Base");
        for template in &templates {
            assert!(
                template.files.iter().any(|(path, _)| *path == "index.pi"),
                "{} has no index.pi",
                template.key
            );
        }
        let files: Vec<&str> = templates[1].files.iter().map(|(path, _)| *path).collect();
        for file in [
            "lib/Scope.pi",
            "lib/Concept.pi",
            "lib/Shape.pi",
            "lib/index.pi",
        ] {
            assert!(files.contains(&file), "{file} in {files:?}");
        }
    }
}
