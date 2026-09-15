//! Compiles the vendored Piton Tree-sitter parser, which has no Rust bindings.

use std::path::Path;

fn main() {
    let src = Path::new("vendor/tree-sitter-piton/src");

    cc::Build::new()
        .include(src)
        .file(src.join("parser.c"))
        .flag_if_supported("-std=c11")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs")
        .compile("tree-sitter-piton");

    println!("cargo:rerun-if-changed={}", src.display());
}
