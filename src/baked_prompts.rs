//! The prompts the application itself gives the harness, written in Piton in
//! `system-prompts` and compiled into the application when it is built (see
//! `build.rs`): a module for each prompt, with a constant for each of its
//! pieces of text. A placeholder in a prompt is a name in angle brackets,
//! such as `<codeRoot>`, filled in with [`fill`].

include!(concat!(env!("OUT_DIR"), "/baked_prompts.rs"));

/// `template` with each `<name>` in `values` replaced with its value.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    values
        .iter()
        .fold(template.to_string(), |text, (name, value)| {
            text.replace(&format!("<{name}>"), value)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_filled() {
        assert_eq!(
            fill("From <a> to <b>, <a>.", &[("a", "here"), ("b", "there")]),
            "From here to there, here."
        );
    }

    /// Each prompt compiled in, with no placeholder left unknown.
    #[test]
    fn prompts_are_baked_in() {
        assert!(divergence::INTRO.contains("<codeRoot>"));
        assert!(divergence::code::TASK.starts_with("Go through every source file"));
        assert_eq!(divergence::spec::ANALYZED, "Spec files to analyze");
        assert!(commit_note::REQUEST.contains("<noNote>"));
        assert!(commit_message::REQUEST.starts_with("Write a git commit message"));
        assert!(file_search::REPLY.contains(r#"{"files": []}"#));
    }
}
