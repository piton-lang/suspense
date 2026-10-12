//! What each mode reaches of each part of a project, as the settings' Who
//! can see what grid shows it: worked out from the profiles as they compile
//! now, so it follows the classification, the project's own areas and
//! levels, and the Spec tasks switches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    Access, Classification, Compiled, Policy, ProfileKind, Refused, Rule, RunFiles, compile,
    profile, refused, relative,
};

/// The parts of a project the grid shows each mode's reach into, as the
/// SettingsWindowScope's areas say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Code,
    SpecSource,
    Reference,
    ProjectData,
    /// An area the project added, by its place among them.
    Added(usize),
    EverythingElse,
}

impl Area {
    /// Its name in `.suspense/sandbox.json`; an added area's levels are
    /// kept with it.
    pub fn key(self) -> &'static str {
        match self {
            Area::Code => "code",
            Area::SpecSource => "spec",
            Area::Reference => "reference",
            Area::ProjectData => "data",
            Area::Added(_) => "",
            Area::EverythingElse => "rest",
        }
    }

    /// Its name as the grid shows it.
    pub fn label(self, classification: &Classification) -> String {
        match self {
            Area::Code => "Code".into(),
            Area::SpecSource => "Spec source".into(),
            Area::Reference => "Compiled reference".into(),
            Area::ProjectData => "Suspense data".into(),
            Area::Added(index) => classification
                .areas
                .get(index)
                .map_or_else(String::new, |area| area.name.clone()),
            Area::EverythingElse => "Other files".into(),
        }
    }
}

/// A level a project sets for a mode in an area, over its profile's default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    Edit,
    Read,
    #[serde(rename = "none")]
    NoAccess,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Edit, Level::Read, Level::NoAccess];

    pub fn label(self) -> &'static str {
        match self {
            Level::Edit => "Edit",
            Level::Read => "Read",
            Level::NoAccess => "No access",
        }
    }
}

/// A profile's name in `.suspense/sandbox.json`, as its mode is called.
pub fn mode_key(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Code => "code",
        ProfileKind::Spec => "spec",
        ProfileKind::Question => "ask",
        ProfileKind::Freeform => "freeform",
        ProfileKind::Bare => "bare",
    }
}

/// The levels a project sets, by mode and area, as `.suspense/sandbox.json`
/// keeps them.
pub type Levels = BTreeMap<String, BTreeMap<String, Level>>;

/// Whether `kind` may be given `level` in `area`, as the SandboxScope says:
/// never past what the sandbox allows. With why not.
pub fn allowed(kind: ProfileKind, area: Area, level: Level) -> Result<(), &'static str> {
    match (kind, area, level) {
        (ProfileKind::Code, Area::SpecSource, Level::Edit | Level::Read) => Err(
            "The separation of code and spec is never relaxed: a Code run can't open the spec's source.",
        ),
        (ProfileKind::Spec, Area::Code, Level::Edit) => Err(
            "The separation of code and spec is never relaxed: a Spec run can't change the code.",
        ),
        (ProfileKind::Question, _, Level::Edit) => Err("Ask can only read, never change anything."),
        (ProfileKind::Freeform, Area::ProjectData, Level::Edit) => Ok(()),
        (_, Area::ProjectData, Level::Edit) => Err("Only Freeform may change Suspense data."),
        _ => Ok(()),
    }
}

/// The level `kind` is set to in `area` by the project, if it set one.
pub fn level(classification: &Classification, kind: ProfileKind, area: Area) -> Option<Level> {
    match area {
        Area::Added(index) => classification
            .areas
            .get(index)?
            .levels
            .get(mode_key(kind))
            .copied(),
        area => classification
            .levels
            .get(mode_key(kind))?
            .get(area.key())
            .copied(),
    }
}

/// Applies the areas and levels the project set for `kind` over its default
/// `policy`, as the SandboxScope's classification says: each area set given
/// that level and nothing else of it, an added area with no level of its
/// own following Other files, the rest of the profile as it was, and the
/// files a run is given on its own, `own`, kept. A level past what the
/// sandbox allows refuses.
pub(super) fn apply(
    policy: &mut Policy,
    kind: ProfileKind,
    classification: &Classification,
    areas: &[(Area, Vec<PathBuf>)],
    own: &[PathBuf],
) -> Result<(), Refused> {
    let set: Vec<(Area, Level)> = areas
        .iter()
        .filter_map(|(area, _)| Some((*area, level(classification, kind, *area)?)))
        .chain(
            level(classification, kind, Area::EverythingElse)
                .map(|level| (Area::EverythingElse, level)),
        )
        .collect();
    if set.is_empty() {
        return Ok(());
    }
    for (area, level) in &set {
        allowed(kind, *area, *level).map_err(|why| {
            refused(format!(
                "the project gives {} runs {} in {}, which the sandbox never allows. {why}",
                kind.label(),
                level.label(),
                area.label(classification)
            ))
        })?;
    }
    // An added area with no level of its own is Other files'.
    let areas: Vec<&(Area, Vec<PathBuf>)> = areas
        .iter()
        .filter(|(area, _)| {
            !matches!(area, Area::Added(_)) || set.iter().any(|(known, _)| known == area)
        })
        .collect();
    let project = policy.project.clone();
    let paths_of = |area: Area| -> Vec<PathBuf> {
        areas
            .iter()
            .find(|(known, _)| *known == area)
            .map(|(_, paths)| paths.clone())
            .unwrap_or_default()
    };
    let named: Vec<PathBuf> = areas.iter().flat_map(|(_, paths)| paths.clone()).collect();
    // A grant is Other files' where it lies in no named area.
    let is_rest = |path: &Path| {
        path.starts_with(&project) && !named.iter().any(|named| path.starts_with(named))
    };
    let reaches = |grant: &Path, paths: &[PathBuf]| {
        paths
            .iter()
            .any(|path| path.starts_with(grant) || grant.starts_with(path))
    };
    // What each area gets by default, before anything is changed.
    let defaults: Vec<(Area, Option<Access>)> = areas
        .iter()
        .map(|(area, paths)| {
            let access = policy
                .grants
                .iter()
                .filter(|(grant, _)| {
                    grant.starts_with(&project) && !own.contains(grant) && reaches(grant, paths)
                })
                .map(|(_, access)| *access)
                .max();
            (*area, access)
        })
        .collect();
    let set_area = |area: Area| set.iter().any(|(known, _)| *known == area);
    for (area, level) in &set {
        let access = match level {
            Level::Edit => Some(Access::ReadWrite),
            Level::Read => Some(Access::ReadOnly),
            Level::NoAccess => None,
        };
        if *area == Area::EverythingElse {
            let removed: Vec<PathBuf> = policy
                .grants
                .iter()
                .filter(|(grant, _)| is_rest(grant) && !own.contains(grant))
                .map(|(grant, _)| grant.clone())
                .collect();
            policy.grants.retain(|(grant, _)| !removed.contains(grant));
            // An area the rest held, not itself set, keeps what it had.
            for (other, default) in &defaults {
                let paths = paths_of(*other);
                if !set_area(*other)
                    && let Some(default) = default
                    && removed
                        .iter()
                        .any(|grant| paths.iter().any(|path| path.starts_with(grant)))
                {
                    for path in paths {
                        policy.grants.push((path, *default));
                    }
                }
            }
            if let Some(access) = access {
                policy.grants.push((project.clone(), access));
                // Never reaching the areas but through their own grants.
                for path in &named {
                    if !policy.excluded.contains(path) {
                        policy.excluded.push(path.clone());
                    }
                }
            }
            continue;
        }
        let paths = paths_of(*area);
        policy.grants.retain(|(grant, _)| {
            own.contains(grant) || !paths.iter().any(|path| grant.starts_with(path))
        });
        match access {
            Some(access) => {
                for path in &paths {
                    policy.grants.push((path.clone(), access));
                }
            }
            // Denied, but for Suspense data, which no wider grant reaches
            // anyway and holds a run's own files, and an area holding
            // another's grants.
            None if *area != Area::ProjectData => {
                for path in &paths {
                    let holds_grant = policy
                        .grants
                        .iter()
                        .any(|(grant, _)| grant.starts_with(path) && grant != path);
                    if !holds_grant && !policy.denied.contains(path) {
                        policy.denied.push(path.clone());
                    }
                }
            }
            None => {}
        }
    }
    Ok(())
}

/// How far a mode reaches into an area: the widest access it has to any of
/// it, none where it reaches none of it, and the paths it reaches.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reach {
    pub access: Option<Access>,
    pub paths: Vec<String>,
}

/// What every mode reaches outside the project, as the grid says beneath.
pub const OUTSIDE: &str = "Outside the project, every mode reads the system and toolchains, and can change only the harness's folders, build caches, and a temporary folder of its own.";

/// What the grid notes beneath Suspense data.
pub const OWN_FILES: &str =
    "Each run is still given its own files here, as its understanding file and the fluency files.";

fn of(rules: &[&Rule], compiled: &Compiled) -> Reach {
    Reach {
        access: rules.iter().map(|rule| rule.access).max(),
        paths: rules
            .iter()
            .map(|rule| relative(&compiled.project, &rule.path))
            .collect(),
    }
}

/// The project's areas, in the grid's order, with what each covers,
/// absolute: Code, Spec source, Compiled reference, Suspense data, then
/// each added area; Other files covers what none of them does.
fn areas_of(
    classification: &Classification,
    agent: crate::agent::Agent,
    project: &Path,
) -> Vec<(Area, Vec<PathBuf>)> {
    let (code, spec) = classification.resolve(project).unwrap_or_default();
    let resolved = |paths: Vec<PathBuf>| -> Vec<PathBuf> {
        paths
            .into_iter()
            .map(|path| dunce::canonicalize(&path).unwrap_or(path))
            .collect()
    };
    let mut areas = vec![
        (Area::Code, resolved(code)),
        (Area::SpecSource, resolved(spec)),
        (
            Area::Reference,
            resolved(vec![project.join(agent.directory()).join("reference")]),
        ),
        (
            Area::ProjectData,
            resolved(vec![project.join(crate::hidden_anchor::APP_DIR)]),
        ),
    ];
    for (index, area) in classification.areas.iter().enumerate() {
        areas.push((
            Area::Added(index),
            resolved(Classification::area_paths(area, project)),
        ));
    }
    areas
}

/// Each row of the grid: its area, its name, and what it covers, as
/// written, Other files listed as the top-level files and folders it holds.
pub fn rows(agent: crate::agent::Agent, project_dir: &Path) -> Vec<(Area, String, Vec<String>)> {
    let project = dunce::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let classification = Classification::load(project_dir);
    let written = |roots: &[String], patterns: &[String]| -> Vec<String> {
        roots
            .iter()
            .map(|root| {
                if project.join(root).is_dir() && !root.ends_with('/') {
                    format!("{root}/")
                } else {
                    root.clone()
                }
            })
            .chain(patterns.iter().cloned())
            .collect()
    };
    let mut rows = vec![
        (
            Area::Code,
            Area::Code.label(&classification),
            written(&classification.code_roots, &classification.code_patterns),
        ),
        (
            Area::SpecSource,
            Area::SpecSource.label(&classification),
            written(&classification.spec_roots, &classification.spec_patterns),
        ),
        (
            Area::Reference,
            Area::Reference.label(&classification),
            vec![format!("{}/reference/", agent.directory())],
        ),
        (
            Area::ProjectData,
            Area::ProjectData.label(&classification),
            vec![format!("{}/", crate::hidden_anchor::APP_DIR)],
        ),
    ];
    for (index, area) in classification.areas.iter().enumerate() {
        rows.push((
            Area::Added(index),
            area.name.clone(),
            written(&area.roots, &area.patterns),
        ));
    }
    // Other files: the top-level files and folders none of the areas holds
    // whole.
    let areas = areas_of(&classification, agent, &project);
    let held: Vec<&PathBuf> = areas.iter().flat_map(|(_, paths)| paths).collect();
    let mut rest: Vec<String> = std::fs::read_dir(&project)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.file_name().is_some_and(|name| name != ".git"))
        .filter(|path| !held.iter().any(|held| path.starts_with(held)))
        .map(|path| {
            let name = relative(&project, &path);
            if path.is_dir() {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    rest.sort();
    rows.push((
        Area::EverythingElse,
        Area::EverythingElse.label(&classification),
        rest,
    ));
    rows
}

/// What a run of each of `kinds` reaches of each area of the project at
/// `project_dir`: a row per area, in the order of [`rows`], a reach per
/// kind; where a kind's profile can't be compiled, why instead.
pub fn reach(
    kinds: &[ProfileKind],
    agent: crate::agent::Agent,
    project_dir: &Path,
) -> Vec<(Area, Vec<Result<Reach, String>>)> {
    let project = dunce::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let classification = Classification::load(project_dir);
    let areas = areas_of(&classification, agent, &project);
    // The files every run is given on its own, which no area's level is.
    let own: Vec<PathBuf> = [
        crate::piton_fluency::file(&project),
        crate::suspense_fluency::file(&project),
    ]
    .into_iter()
    .map(|path| dunce::canonicalize(&path).unwrap_or(path))
    .collect();
    let compiled: Vec<Result<Compiled, String>> = kinds
        .iter()
        .map(|kind| {
            profile(*kind, agent, project_dir, &RunFiles::default())
                .and_then(|policy| compile(&policy))
                .map_err(|refused| refused.message)
        })
        .collect();
    let mut rows = Vec::new();
    for (area, paths) in &areas {
        let cells = compiled
            .iter()
            .map(|compiled| {
                let compiled = compiled.as_ref().map_err(Clone::clone)?;
                // What covers the area, or lies within it, but for what is
                // denied and a run's own files.
                let rules: Vec<&Rule> = compiled
                    .rules
                    .iter()
                    .filter(|rule| {
                        rule.path.starts_with(&compiled.project) && !own.contains(&rule.path)
                    })
                    .filter(|rule| {
                        paths.iter().any(|path| {
                            !compiled
                                .denied
                                .iter()
                                .any(|denied| path.starts_with(denied))
                                && (path.starts_with(&rule.path) || rule.path.starts_with(path))
                        })
                    })
                    .collect();
                Ok(of(&rules, compiled))
            })
            .collect();
        rows.push((*area, cells));
    }
    // Other files: what is granted in the project outside the areas, but
    // for the guidance every run reads.
    let all: Vec<&PathBuf> = areas.iter().flat_map(|(_, paths)| paths).collect();
    let cells = compiled
        .iter()
        .map(|compiled| {
            let compiled = compiled.as_ref().map_err(Clone::clone)?;
            let rules: Vec<&Rule> = compiled
                .rules
                .iter()
                .filter(|rule| rule.path.starts_with(&compiled.project))
                .filter(|rule| !all.iter().any(|path| rule.path.starts_with(path)))
                .filter(|rule| {
                    !matches!(
                        rule.path.file_name().and_then(|name| name.to_str()),
                        Some("CLAUDE.md" | "AGENTS.md" | ".mcp.json")
                    )
                })
                .collect();
            Ok(of(&rules, compiled))
        })
        .collect();
    rows.push((Area::EverythingElse, cells));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid says each mode's reach as its profile compiles: Code edits
    /// the code and can't open the spec, Spec the other way round, Ask reads
    /// both, Freeform edits both.
    #[test]
    fn each_mode_reaches_what_its_profile_grants() {
        let dir = std::env::temp_dir().join(format!("suspense-reach-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        for folder in ["src", "spec", ".claude/reference", ".suspense"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        std::fs::write(dir.join("README.md"), "readme\n").unwrap();
        Classification {
            code_roots: vec!["src".into()],
            spec_roots: vec!["spec".into()],
            ..Classification::default()
        }
        .save(&dir)
        .unwrap();
        let kinds = [
            ProfileKind::Code,
            ProfileKind::Spec,
            ProfileKind::Question,
            ProfileKind::Freeform,
        ];
        let rows = reach(&kinds, crate::agent::Agent::Claude, &dir);
        let access = |area: Area| -> Vec<Option<Access>> {
            rows.iter()
                .find(|(row, _)| *row == area)
                .unwrap()
                .1
                .iter()
                .map(|cell| cell.as_ref().unwrap().access)
                .collect()
        };
        use Access::{ReadOnly as R, ReadWrite as W};
        assert_eq!(access(Area::Code), [Some(W), None, Some(R), Some(W)]);
        assert_eq!(access(Area::SpecSource), [None, Some(W), Some(R), Some(W)]);
        assert_eq!(
            access(Area::Reference),
            [Some(R), Some(W), Some(R), Some(W)]
        );
        // The harness folder around the reference is the rest of the
        // project: Code and Ask read it, Spec edits it.
        assert_eq!(
            access(Area::EverythingElse),
            [Some(R), Some(W), Some(R), Some(W)]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The project's own levels apply over the defaults, and one past what
    /// the sandbox allows refuses the profile, so no run starts.
    #[test]
    fn levels_apply_over_the_defaults_and_never_past_the_limits() {
        let dir = std::env::temp_dir().join(format!("suspense-levels-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        for folder in ["src", "spec", ".claude/reference", ".suspense", "docs"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        let save = |levels: &[(&str, &str, Level)]| {
            let mut set = Levels::new();
            for (mode, area, level) in levels {
                set.entry(mode.to_string())
                    .or_default()
                    .insert(area.to_string(), *level);
            }
            Classification {
                code_roots: vec!["src".into()],
                spec_roots: vec!["spec".into()],
                levels: set,
                ..Classification::default()
            }
            .save(&dir)
            .unwrap();
        };
        let reached = |kind: ProfileKind, area: Area| -> Result<Option<Access>, String> {
            let rows = reach(&[kind], crate::agent::Agent::Claude, &dir);
            let row = rows.into_iter().find(|(row, _)| *row == area).unwrap();
            row.1.into_iter().next().unwrap().map(|reach| reach.access)
        };
        // Taken from the Code run, the reference; given to it, the rest.
        save(&[
            ("code", "reference", Level::NoAccess),
            ("code", "rest", Level::Read),
        ]);
        assert_eq!(reached(ProfileKind::Code, Area::Reference), Ok(None));
        assert_eq!(
            reached(ProfileKind::Code, Area::EverythingElse),
            Ok(Some(Access::ReadOnly))
        );
        assert_eq!(
            reached(ProfileKind::Code, Area::Code),
            Ok(Some(Access::ReadWrite))
        );
        assert_eq!(
            reached(ProfileKind::Code, Area::SpecSource),
            Ok(None),
            "the rest reached the spec"
        );
        // A Freeform run kept off the rest of the project.
        save(&[("freeform", "rest", Level::NoAccess)]);
        assert_eq!(
            reached(ProfileKind::Freeform, Area::EverythingElse),
            Ok(None)
        );
        assert_eq!(
            reached(ProfileKind::Freeform, Area::Code),
            Ok(Some(Access::ReadWrite))
        );
        // Past the limits: refused, never applied.
        for (mode, area, kind) in [
            ("code", "spec", ProfileKind::Code),
            ("ask", "code", ProfileKind::Question),
            ("code", "data", ProfileKind::Code),
        ] {
            let level = if mode == "code" && area == "spec" {
                Level::Read
            } else {
                Level::Edit
            };
            save(&[(mode, area, level)]);
            assert!(
                profile(
                    kind,
                    crate::agent::Agent::Claude,
                    &dir,
                    &RunFiles::default()
                )
                .is_err(),
                "{mode} {area} {level:?} wasn't refused"
            );
        }
        save(&[("spec", "code", Level::Edit)]);
        assert!(
            profile(
                ProfileKind::Spec,
                crate::agent::Agent::Claude,
                &dir,
                &RunFiles::default()
            )
            .is_err()
        );
        assert!(allowed(ProfileKind::Spec, Area::Code, Level::Read).is_ok());
        // Only Freeform may change Suspense data.
        assert!(allowed(ProfileKind::Freeform, Area::ProjectData, Level::Edit).is_ok());
        assert!(allowed(ProfileKind::Spec, Area::ProjectData, Level::Edit).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An added area is a row of its own, taken out of Other files, with
    /// its own levels, Other files' where it has none; one overlapping
    /// another area refuses every run.
    #[test]
    fn added_areas_have_levels_of_their_own() {
        let dir = std::env::temp_dir().join(format!("suspense-added-areas-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        for folder in ["src", "spec", ".claude/reference", ".suspense", "docs"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        std::fs::write(dir.join("README.md"), "readme\n").unwrap();
        let save = |area: crate::sandbox::AddedArea, levels: Levels| {
            Classification {
                code_roots: vec!["src".into()],
                spec_roots: vec!["spec".into()],
                levels,
                areas: vec![area],
                ..Classification::default()
            }
            .save(&dir)
            .unwrap();
        };
        let docs = |levels: &[(&str, Level)]| crate::sandbox::AddedArea {
            name: "Docs".into(),
            roots: vec!["docs".into()],
            patterns: Vec::new(),
            levels: levels
                .iter()
                .map(|(mode, level)| (mode.to_string(), *level))
                .collect(),
        };
        let reached = |kind: ProfileKind, area: Area| -> Option<Access> {
            reach(&[kind], crate::agent::Agent::Claude, &dir)
                .into_iter()
                .find(|(row, _)| *row == area)
                .unwrap()
                .1
                .into_iter()
                .next()
                .unwrap()
                .unwrap()
                .access
        };
        // Its own level for Code: Read.
        save(docs(&[("code", Level::Read)]), Levels::new());
        assert_eq!(
            reached(ProfileKind::Code, Area::Added(0)),
            Some(Access::ReadOnly)
        );
        let rows = rows(crate::agent::Agent::Claude, &dir);
        let names: Vec<&str> = rows.iter().map(|(_, name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Code",
                "Spec source",
                "Compiled reference",
                "Suspense data",
                "Docs",
                "Other files"
            ]
        );
        let other = &rows.last().unwrap().2;
        assert!(
            other.contains(&"README.md".to_string()) && !other.contains(&"docs/".to_string()),
            "{other:?}"
        );
        // With none, Other files' level.
        let mut rest = Levels::new();
        rest.entry("code".into())
            .or_default()
            .insert("rest".into(), Level::Read);
        save(docs(&[]), rest);
        assert_eq!(
            reached(ProfileKind::Code, Area::Added(0)),
            Some(Access::ReadOnly)
        );
        // Suspense data is No access by default, a run's own files aside.
        assert_eq!(reached(ProfileKind::Code, Area::ProjectData), None);
        // Overlapping the code: refused.
        save(
            crate::sandbox::AddedArea {
                name: "Sources".into(),
                roots: vec!["src/inner".into()],
                ..crate::sandbox::AddedArea::default()
            },
            Levels::new(),
        );
        assert!(
            profile(
                ProfileKind::Code,
                crate::agent::Agent::Claude,
                &dir,
                &RunFiles::default()
            )
            .is_err()
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
