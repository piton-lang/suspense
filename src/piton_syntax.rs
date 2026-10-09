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

/// Piton's Tree-sitter language, for reading a file's structure.
pub fn language() -> tree_sitter::Language {
    LANGUAGE.into()
}

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

/// The name at byte `offset` of Piton `text`, as hovering shows it: a key or
/// property name, an anchor's name where it is declared or extended, a
/// keyword, a name an import brings in, or a name inside an expression in
/// braces, plain or after `$`, `#`, or `@`. None over anything else: prose,
/// numbers, punctuation, indentation, comments, and blank space.
pub fn hovered_name(text: &str, offset: usize) -> Option<std::ops::Range<usize>> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    if !text.get(offset..)?.chars().next().is_some_and(is_word) {
        return None;
    }
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&LANGUAGE.into()).ok()?;
    let tree = parser.parse(text, None)?;
    let node = tree
        .root_node()
        .descendant_for_byte_range(offset, offset + 1)?;
    let name = if node.is_named() {
        matches!(
            node.kind(),
            "key"
                | "identifier"
                | "declaration_keyword"
                | "keyword_name"
                | "builtin_type"
                | "self_reference"
                | "constant"
                | "pass_statement"
        )
    } else {
        // An anonymous token spelled as a word is a keyword.
        node.kind().chars().all(|c| c.is_ascii_alphabetic())
    };
    if !name {
        return None;
    }
    // The name alone, without a key's colon.
    let range = node.byte_range();
    let start = range.start
        + text[range.clone()]
            .find(is_word)
            .unwrap_or(0);
    let end = text[..range.end]
        .rfind(is_word)
        .map_or(range.end, |at| at + text[at..].chars().next().map_or(1, char::len_utf8));
    (start <= offset && offset < end).then_some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names are hovered, and nothing else is: not prose, numbers,
    /// punctuation, indentation, comments, or blank space.
    #[test]
    fn only_names_are_hovered() {
        let text = "use /lib\nfrom ./a import A\n\nexport scope ApplicationScope extends Base:\n    pitch: hello world @{A.b} and ${d} {f + 2}\n    total:: number: {1 + 2}\n";
        let at = |word: &str, nth: usize| text.match_indices(word).nth(nth).unwrap().0;
        let name = |offset: usize| hovered_name(text, offset).map(|range| &text[range]);
        // A key, without its colon; an anchor's name, declared and
        // extended; keywords; an imported name; names in expressions.
        assert_eq!(name(at("pitch", 0) + 2), Some("pitch"));
        assert_eq!(name(at("ApplicationScope", 0) + 3), Some("ApplicationScope"));
        assert_eq!(name(at("Base", 0)), Some("Base"));
        assert_eq!(name(at("export", 0)), Some("export"));
        assert_eq!(name(at("extends", 0) + 1), Some("extends"));
        assert_eq!(name(at("import", 0)), Some("import"));
        assert_eq!(name(at("import A", 0) + 7), Some("A"));
        assert_eq!(name(at("b}", 0)), Some("b"));
        assert_eq!(name(at("d}", 0)), Some("d"));
        assert_eq!(name(at("f +", 0)), Some("f"));
        assert_eq!(name(at("number", 0)), Some("number"));
        // Prose, numbers, punctuation, indentation, and blank space.
        assert_eq!(name(at("hello", 0)), None);
        assert_eq!(name(at("world", 0) + 2), None);
        assert_eq!(name(at("and", 0)), None);
        assert_eq!(name(at("2}", 0)), None);
        assert_eq!(name(at("pitch:", 0) + 5), None);
        assert_eq!(name(at("    pitch", 0)), None);
        assert_eq!(name(at("@{", 0)), None);
        assert_eq!(name(text.len() - 1), None);
    }

    #[test]
    fn highlight_query_matches_the_parser() {
        let language: tree_sitter::Language = LANGUAGE.into();
        tree_sitter::Query::new(&language, HIGHLIGHTS).unwrap();

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser
            .parse(
                "use /lib\nfrom ./a import A\n\nexport scope ApplicationScope:\n    pitch: hello @{A}\n    total:: number: {1 + 2}\n    text:\n        \\\\\\\n        {kept}\n        \\\\\\\n",
                None,
            )
            .unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
    }
}
