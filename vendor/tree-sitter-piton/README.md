Vendored from https://github.com/piton-lang/tree-sitter-piton at commit 3b1522f41abfd397f95071495f41ca479ef7c2a8 (MIT).

Only the generated parser and the queries used for highlighting are copied.
The grammar has no Rust bindings, so `build.rs` compiles `src/parser.c`.
