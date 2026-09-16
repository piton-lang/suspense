//! Generating skills: maps the spec's scopes and how they reference each
//! other, has the harness rank them for the skills worth building, and writes
//! the skills chosen into the spec, each building its scope.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;

use crate::baked_prompts::{fill, generate_skills};

/// A scope the spec exports, and how it's connected.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scope {
    pub name: String,
    /// Its file, relative to the project.
    pub file: String,
    /// Where it's imported from: its file within the spec location, without
    /// .pi or a trailing /index.
    pub module: String,
    /// The scopes that reference it, and those it references, with how many
    /// times.
    pub used_by: BTreeMap<String, usize>,
    pub uses: BTreeMap<String, usize>,
    pub has_skill: bool,
}

/// The spec's scopes, by name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpecMap {
    pub spec_root: String,
    pub scopes: BTreeMap<String, Scope>,
    /// Whether the spec has the build-scope keyword to write skills with.
    pub has_build_scope: bool,
}

/// The names `text` references with an at-brace or a dollar-brace, each by
/// the name it starts with.
pub fn references(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(['@', '$']) {
        let after = &rest[at + 1..];
        if let Some(body) = after.strip_prefix('{') {
            let name: String = body
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                names.push(name);
            }
        }
        rest = after;
    }
    names
}

/// The names `text` imports with `from … import A, B`.
fn imports(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("from ")?.split_once(" import "))
        .flat_map(|(_, names)| names.split(',').map(|name| name.trim().to_string()))
        .filter(|name| !name.is_empty())
        .collect()
}

/// The scopes `text` exports.
fn exported_scopes(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.strip_prefix("export scope "))
        .filter_map(|rest| {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// Where the file `file`, within `spec_root`, is imported from.
pub fn module_of(file: &str, spec_root: &str) -> String {
    let within = file
        .strip_prefix(spec_root)
        .unwrap_or(file)
        .trim_start_matches('/');
    let without = within.strip_suffix(".pi").unwrap_or(within);
    let without = without
        .strip_suffix("/index")
        .or_else(|| (without == "index").then_some(""))
        .unwrap_or(without);
    format!("/{without}")
}

/// Maps `files`, each relative to `project_dir`, whose spec location is
/// `spec_root`.
pub fn map(project_dir: &Path, spec_root: &str, files: &[String]) -> SpecMap {
    let texts: Vec<(String, String)> = files
        .iter()
        .filter_map(|file| {
            Some((
                file.clone(),
                std::fs::read_to_string(project_dir.join(file)).ok()?,
            ))
        })
        .collect();
    let mut scopes: BTreeMap<String, Scope> = BTreeMap::new();
    for (file, text) in &texts {
        for name in exported_scopes(text) {
            scopes.entry(name.clone()).or_insert_with(|| Scope {
                name,
                file: file.clone(),
                module: module_of(file, spec_root),
                ..Scope::default()
            });
        }
    }
    let skills_dir = format!("{spec_root}/agent/skills/");
    let mut with_skills = BTreeSet::new();
    for (file, text) in &texts {
        if let Some(skill_file) = file.strip_prefix(&skills_dir) {
            // A skill builds a scope that is its scope, or that it's named
            // for.
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix("scope:") {
                    with_skills.extend(references(&rest.replace('{', "@{")));
                }
            }
            let skill = skill_file.strip_suffix(".pi").unwrap_or(skill_file);
            with_skills.extend(
                imports(text)
                    .into_iter()
                    .filter(|name| skill_name(name) == skill),
            );
            continue;
        }
        let owners = exported_scopes(text);
        for referenced in references(text) {
            if !scopes.contains_key(&referenced) {
                continue;
            }
            for owner in owners.iter().filter(|owner| **owner != referenced) {
                *scopes
                    .get_mut(owner)
                    .unwrap()
                    .uses
                    .entry(referenced.clone())
                    .or_default() += 1;
                *scopes
                    .get_mut(&referenced)
                    .unwrap()
                    .used_by
                    .entry(owner.clone())
                    .or_default() += 1;
            }
        }
    }
    for scope in scopes.values_mut() {
        scope.has_skill = with_skills.contains(&scope.name);
    }
    SpecMap {
        spec_root: spec_root.to_string(),
        scopes,
        has_build_scope: project_dir
            .join(spec_root)
            .join("lib/agent/skills/BuildScope.pi")
            .is_file(),
    }
}

/// The prompt ranking `map`'s scopes: its paragraphs from
/// `system-prompts/generate-skills.pi`, then the map.
pub fn prompt(map: &SpecMap) -> String {
    let rows: Vec<String> = map
        .scopes
        .values()
        .map(|scope| {
            format!(
                "- {} ({}): used by {}, uses {}{}",
                scope.name,
                scope.file,
                scope.used_by.len(),
                scope.uses.len(),
                if scope.has_skill {
                    ", already has a skill"
                } else {
                    ""
                }
            )
        })
        .collect();
    format!(
        "{}\n\n{}\n\n{}\n\n{}:\n{}\n",
        fill(generate_skills::INTRO, &[("specRoot", &map.spec_root)]),
        generate_skills::TASK,
        generate_skills::REPLY_FORMAT,
        generate_skills::MAP,
        rows.join("\n")
    )
}

/// What the harness said of a scope.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Ranked {
    pub scope: String,
    #[serde(default)]
    pub worth: f32,
    #[serde(default)]
    pub should: bool,
    #[serde(default)]
    pub why: String,
}

#[derive(Deserialize)]
struct Reply {
    skills: Vec<Ranked>,
}

/// The ranking in what the harness replied, which may have a little around
/// its JSON.
pub fn parse_reply(text: &str) -> Result<Vec<Ranked>> {
    let start = text
        .find('{')
        .ok_or_else(|| anyhow!("the reply has no JSON in it"))?;
    let end = text
        .rfind('}')
        .ok_or_else(|| anyhow!("the reply has no JSON in it"))?;
    let reply: Reply =
        serde_json::from_str(&text[start..=end]).context("the reply isn't the JSON asked for")?;
    Ok(reply.skills)
}

/// A scope offered for a skill.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub name: String,
    /// How much a skill is worth, from 0 to 1, when the harness said.
    pub worth: Option<f32>,
    pub should: bool,
    pub why: String,
}

/// Every scope offered: those `ranked` names that the map has, most worth
/// first, then the others, those used by the most first, then those using
/// the most.
pub fn candidates(map: &SpecMap, ranked: &[Ranked]) -> Vec<Candidate> {
    let mut named: Vec<Candidate> = ranked
        .iter()
        .filter(|ranked| map.scopes.contains_key(&ranked.scope))
        .map(|ranked| Candidate {
            name: ranked.scope.clone(),
            worth: Some(ranked.worth.clamp(0., 1.)),
            should: ranked.should,
            why: ranked.why.trim().to_string(),
        })
        .collect();
    named.sort_by(|a, b| b.worth.unwrap_or(0.).total_cmp(&a.worth.unwrap_or(0.)));
    named.dedup_by(|a, b| a.name == b.name);
    let mut rest: Vec<&Scope> = map
        .scopes
        .values()
        .filter(|scope| !named.iter().any(|named| named.name == scope.name))
        .collect();
    rest.sort_by(|a, b| {
        b.used_by
            .len()
            .cmp(&a.used_by.len())
            .then(b.uses.len().cmp(&a.uses.len()))
            .then(a.name.cmp(&b.name))
    });
    named.extend(rest.into_iter().map(|scope| Candidate {
        name: scope.name.clone(),
        worth: None,
        should: false,
        why: String::new(),
    }));
    named
}

/// The name of the skill building `scope`.
pub fn skill_name(scope: &str) -> String {
    let base = scope
        .strip_suffix("Scope")
        .filter(|base| !base.is_empty())
        .unwrap_or(scope);
    let base: String = base.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    format!("Build{base}")
}

/// The source of the skill building `scope`.
pub fn skill_source(scope: &Scope, build_scope: bool) -> String {
    let skill = skill_name(&scope.name);
    let name = &scope.name;
    let module = &scope.module;
    if build_scope {
        format!(
            "use /lib/agent/skills/BuildScope\n\nfrom {module} import {name}\n\nexport build-scope {skill}:\n    scope: {{{name}}}\n"
        )
    } else {
        format!(
            "use @piton/belay\n\nfrom {module} import {name}\n\nexport skill {skill}:\n    description: build or change the ${{{name}}} scope\n    useWhen: you need to build or change the ${{{name}}} scope\n    prompt:\n        Read @{{{name}}} and the specs it references, then build or change\n        it according to what already exists in the codebase. The spec is the\n        source of truth: where the code differs from it, rework the code to\n        match.\n"
        )
    }
}

/// Writes a skill into the spec for each of `names`, never over a file
/// already there, and exports it. Returns, for each, the file written or why
/// it wasn't.
pub fn write_skills(
    project_dir: &Path,
    map: &SpecMap,
    names: &[String],
) -> Vec<(String, Result<String>)> {
    let spec = project_dir.join(&map.spec_root);
    let agent = spec.join("agent");
    let skills = agent.join("skills");
    let agent_is_new = !agent.exists();
    let mut results = Vec::new();
    let mut exported = Vec::new();
    for name in names {
        let result = (|| {
            let scope = map
                .scopes
                .get(name)
                .ok_or_else(|| anyhow!("{name} isn't a scope in the spec"))?;
            let file_name = format!("{}.pi", skill_name(name));
            let file = skills.join(&file_name);
            if file.exists() {
                bail!("{} is already there", file.display());
            }
            std::fs::create_dir_all(&skills)
                .with_context(|| format!("couldn't create {}", skills.display()))?;
            std::fs::write(&file, skill_source(scope, map.has_build_scope))
                .with_context(|| format!("couldn't write {}", file.display()))?;
            exported.push(skill_name(name));
            Ok(format!("{}/agent/skills/{file_name}", map.spec_root))
        })();
        results.push((name.clone(), result));
    }
    if exported.is_empty() {
        return results;
    }
    let append = |file: &Path, line: &str| -> Result<()> {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        if text.lines().any(|existing| existing.trim() == line) {
            return Ok(());
        }
        let mut text = text;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(line);
        text.push('\n');
        std::fs::write(file, text).with_context(|| format!("couldn't write {}", file.display()))
    };
    let mut indexed = Ok(());
    for skill in &exported {
        indexed = indexed.and_then(|_| {
            append(
                &skills.join("index.pi"),
                &format!("from ./{skill} export *"),
            )
        });
    }
    if agent_is_new {
        indexed = indexed
            .and_then(|_| append(&agent.join("index.pi"), "from ./skills export *"))
            .and_then(|_| {
                let index = spec.join("index.pi");
                if index.is_file() {
                    append(&index, "from ./agent export *")
                } else {
                    Ok(())
                }
            });
    }
    if let Err(err) = indexed {
        for (_, result) in &mut results {
            if result.is_ok() {
                *result = Err(anyhow!("written, but not exported: {err:#}"));
            }
        }
    }
    results
}

/// A project whose spec has the scope, concept, and shape lib and three
/// scopes: A, used twice by B and once by C, and B, used by C. Returns its
/// folder and its spec files.
#[cfg(test)]
pub fn fixture(name: &str) -> (std::path::PathBuf, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("suspense-skills-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let spec = dir.join("spec");
    // The scope, concept, and shape lib, and three scopes on it.
    let template = crate::project_templates::all()
        .into_iter()
        .find(|template| template.key == "scope-concept-shape")
        .unwrap();
    for (path, text) in template
        .files
        .iter()
        .filter(|(path, _)| path.starts_with("lib/"))
    {
        let path = spec.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let scope = |name: &str, imports: &str, uses: &str| {
        format!(
            "use /lib\n{imports}\nexport scope {name}Scope:\n    concept: {{{name}Concept}}\n\nconcept {name}Concept:\n    pitch: {name} {uses}\n    shape: {{{name}Shape}}\n\nshape {name}Shape:\n    description: plain\n"
        )
    };
    for (path, text) in [
        (
            "index.pi".to_string(),
            "use /lib\n\nfrom ./a export *\nfrom ./b export *\nfrom ./c export *\n".to_string(),
        ),
        ("a/index.pi".to_string(), scope("A", "", "")),
        (
            "b/index.pi".to_string(),
            scope("B", "from /a import AScope\n", "@{AScope} and @{AScope}"),
        ),
        (
            "c/index.pi".to_string(),
            scope(
                "C",
                "from /a import AScope\nfrom /b import BScope\n",
                "@{AScope} @{BScope}",
            ),
        ),
    ] {
        let path = spec.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let files: Vec<String> = [
        "spec/index.pi",
        "spec/a/index.pi",
        "spec/b/index.pi",
        "spec/c/index.pi",
    ]
    .map(String::from)
    .to_vec();
    std::fs::write(
        dir.join("piton.config.pi"),
        "use @piton/config\nuse @piton/belay\n\nfrom @piton/belay import ClaudeAdapter\n\nexport piton-config Project:\n    root: ./spec\n    entry: ./spec/index.pi\n\n    frameworks:\n        - {BelayConfiguration}\n\nbelay-config BelayConfiguration:\n    codeRoot: ./src\n\n    adapters:\n        - {ClaudeAdapter}\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    (dir, files)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::{Ranked, candidates, fixture, map, module_of, parse_reply, references, skill_name};

    #[test]
    fn names_references_modules_and_skills() {
        assert_eq!(
            references("See @{App} and ${Ribbon.concept}, not @ {X} or ${1x}."),
            ["App", "Ribbon"]
        );
        assert_eq!(
            module_of("spec/ui/components/ribbon/index.pi", "spec"),
            "/ui/components/ribbon"
        );
        assert_eq!(
            module_of("spec/scope/application/MainWindow.pi", "spec"),
            "/scope/application/MainWindow"
        );
        assert_eq!(module_of("spec/index.pi", "spec"), "/");
        assert_eq!(skill_name("RibbonScope"), "BuildRibbon");
        assert_eq!(skill_name("ProjectIndicator"), "BuildProjectIndicator");
        assert_eq!(skill_name("Scope"), "BuildScope");
        let reply = parse_reply(
            r#"Sure: {"skills": [{"scope": "A", "worth": 0.9, "should": true, "why": "Hub."}]}"#,
        )
        .unwrap();
        assert_eq!(reply[0].scope, "A");
        assert!(parse_reply("nope").is_err());
    }

    /// The spec's scopes are mapped with who uses whom, scopes with skills
    /// noted; they're offered ranked by the harness, then by connections;
    /// and a skill written builds, exported from new agent and skills
    /// folders.
    #[test]
    fn maps_ranks_and_writes_skills_that_build() {
        let (dir, files) = fixture("model");

        let spec_map = map(&dir, "spec", &files);
        let a = &spec_map.scopes["AScope"];
        assert_eq!(a.used_by.get("BScope"), Some(&2));
        assert_eq!(a.used_by.len(), 2);
        assert_eq!(spec_map.scopes["CScope"].uses.len(), 2);
        assert!(!spec_map.has_build_scope);

        let offered = candidates(
            &spec_map,
            &[Ranked {
                scope: "BScope".into(),
                worth: 0.7,
                should: true,
                why: "Joins things.".into(),
            }],
        );
        let names: Vec<&str> = offered.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["BScope", "AScope", "CScope"]);

        let results = super::write_skills(&dir, &spec_map, &["AScope".into()]);
        assert_eq!(
            results[0].1.as_ref().unwrap(),
            "spec/agent/skills/BuildA.pi"
        );
        assert!(
            super::write_skills(&dir, &spec_map, &["AScope".into()])[0]
                .1
                .is_err()
        );
        let files: Vec<String> = [
            "spec/index.pi",
            "spec/a/index.pi",
            "spec/b/index.pi",
            "spec/c/index.pi",
            "spec/agent/index.pi",
            "spec/agent/skills/index.pi",
            "spec/agent/skills/BuildA.pi",
        ]
        .map(String::from)
        .to_vec();
        assert!(map(&dir, "spec", &files).scopes["AScope"].has_skill);

        if !crate::piton_build::piton_missing() {
            let build = Command::new("piton")
                .arg("build")
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                build.status.success(),
                "piton build failed: {}",
                String::from_utf8_lossy(&build.stderr)
            );
            assert!(
                Path::new(&dir.join(".claude/skills/build-a/SKILL.md")).is_file(),
                "the skill wasn't built"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
