Vendored from the piton-rs repository's `editors/tree-sitter-piton` at commit
01d6d80b11ac598fe47b6976623ade9e8df5400d (MIT), published as
https://github.com/piton-lang/tree-sitter-piton.

Only the generated parser and the queries used for highlighting are copied.
The grammar has no Rust bindings, so `build.rs` compiles `src/parser.c`.
