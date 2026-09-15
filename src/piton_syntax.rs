//! Piton syntax highlighting, from the vendored Tree-sitter grammar.

use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};
use tree_sitter_language::LanguageFn;

pub const LANGUAGE_NAME: &str = "piton";

unsafe extern "C" {
    fn tree_sitter_piton() -> *const ();
}

const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_piton) };

const HIGHLIGHTS: &str = include_str!("../vendor/tree-sitter-piton/queries/highlights.scm");
const INJECTIONS: &str = include_str!("../vendor/tree-sitter-piton/queries/injections.scm");

/// Registers Piton with the highlighter so editors can use [`LANGUAGE_NAME`].
pub fn init() {
    let config = LanguageConfig::new(
        LANGUAGE_NAME,
        LANGUAGE.into(),
        Vec::new(),
        HIGHLIGHTS,
        INJECTIONS,
        "",
    );
    LanguageRegistry::singleton().register(LANGUAGE_NAME, &config);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_query_matches_the_parser() {
        let language: tree_sitter::Language = LANGUAGE.into();
        tree_sitter::Query::new(&language, HIGHLIGHTS).unwrap();

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser
            .parse("export scope ApplicationScope:\n    pitch: hello\n", None)
            .unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
    }
}
