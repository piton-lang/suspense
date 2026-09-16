//! Rescoping: has the harness find concepts the spec describes over and over
//! that would be better as scopes of their own, ranks them, and builds the
//! prompt asking the harness to refactor the spec around those chosen.

use std::collections::BTreeSet;

use anyhow::{Context as _, Result, anyhow};
use serde::Deserialize;

use crate::baked_prompts::{fill, rescope};

/// A place the spec describes a concept.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Place {
    pub file: String,
    pub scope: String,
    pub excerpt: String,
}

/// A concept the spec repeats, proposed as a scope of its own.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Concept {
    /// The proposed scope's name.
    pub name: String,
    /// What the concept is.
    pub concept: String,
    /// How much extracting it is worth, from 0 to 1.
    pub worth: f32,
    pub why: String,
    pub places: Vec<Place>,
}

impl Concept {
    /// How many different scopes describe it.
    pub fn scope_count(&self) -> usize {
        self.places
            .iter()
            .map(|place| place.scope.as_str())
            .collect::<BTreeSet<_>>()
            .len()
    }
}

#[derive(Deserialize)]
struct Reply {
    concepts: Vec<Concept>,
}

/// The concepts in what the harness replied, which may have a little around
/// its JSON, ranked: most worth first, then those with the most places.
pub fn parse_reply(text: &str) -> Result<Vec<Concept>> {
    let start = text
        .find('{')
        .ok_or_else(|| anyhow!("the reply has no JSON in it"))?;
    let end = text
        .rfind('}')
        .ok_or_else(|| anyhow!("the reply has no JSON in it"))?;
    let reply: Reply =
        serde_json::from_str(&text[start..=end]).context("the reply isn't the JSON asked for")?;
    let mut concepts: Vec<Concept> = reply
        .concepts
        .into_iter()
        .filter(|concept| !concept.name.trim().is_empty())
        .map(|mut concept| {
            concept.worth = concept.worth.clamp(0., 1.);
            concept
        })
        .collect();
    concepts.sort_by(|a, b| {
        b.worth
            .total_cmp(&a.worth)
            .then(b.places.len().cmp(&a.places.len()))
            .then(a.name.cmp(&b.name))
    });
    Ok(concepts)
}

/// The prompt looking for repeated concepts in `files`, the spec's files
/// within `spec_root`.
pub fn search_prompt(spec_root: &str, files: &[String]) -> String {
    format!(
        "{}\n\n{}\n\n{}\n\n{}:\n{}\n",
        fill(rescope::INTRO, &[("specRoot", spec_root)]),
        rescope::TASK,
        rescope::REPLY_FORMAT,
        rescope::FILES,
        files.join("\n")
    )
}

/// The prompt asking for `concepts` to be refactored into scopes of their own.
pub fn refactor_prompt(concepts: &[Concept]) -> String {
    let mut prompt = format!("{}\n\n{}:", rescope::REFACTOR, rescope::CONCEPTS);
    for concept in concepts {
        prompt.push_str(&format!("\n\n{}: {}\n", concept.name, concept.concept));
        for place in &concept.places {
            prompt.push_str(&format!(
                "- {} in {}: {}\n",
                place.scope,
                place.file,
                place.excerpt.replace('\n', " ")
            ));
        }
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::{parse_reply, refactor_prompt, search_prompt};

    /// Concepts are read from the reply and ranked, most worth first, then by
    /// how many places; the search prompt lists the files; and the refactor
    /// prompt names each concept and every place it's described.
    #[test]
    fn ranks_concepts_and_builds_prompts() {
        let concepts = parse_reply(
            r#"Found: {"concepts": [
                {"name": "TooltipScope", "concept": "Tooltips", "worth": 0.4,
                 "places": [{"file": "spec/a.pi", "scope": "AScope", "excerpt": "a tooltip"}]},
                {"name": "", "worth": 1.0},
                {"name": "SpinnerScope", "concept": "Spinners", "worth": 1.4, "why": "Everywhere.",
                 "places": [
                    {"file": "spec/a.pi", "scope": "AScope", "excerpt": "a spinner"},
                    {"file": "spec/b.pi", "scope": "BScope", "excerpt": "another\nspinner"},
                    {"file": "spec/b.pi", "scope": "BScope", "excerpt": "again"}
                 ]}
            ]}"#,
        )
        .unwrap();
        let names: Vec<&str> = concepts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["SpinnerScope", "TooltipScope"]);
        assert_eq!(concepts[0].worth, 1.);
        assert_eq!(concepts[0].scope_count(), 2);
        assert!(parse_reply("no").is_err());

        let search = search_prompt("spec", &["spec/a.pi".into(), "spec/b.pi".into()]);
        assert!(search.contains("the spec located in spec describes"));
        assert!(search.ends_with("Spec files:\nspec/a.pi\nspec/b.pi\n"));

        let refactor = refactor_prompt(&concepts[..1]);
        assert!(refactor.contains("SpinnerScope: Spinners"));
        assert!(refactor.contains("- BScope in spec/b.pi: another spinner"));
    }
}
