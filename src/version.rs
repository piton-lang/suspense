//! The application's own version, baked in when it is built, as the
//! EdgeReleasesScope says: an edge release's `0.1.N`, given by its workflow
//! as `SUSPENSE_VERSION`, and otherwise the version Cargo.toml gives.

/// The application's version.
pub const VERSION: &str = match option_env!("SUSPENSE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// What `suspense --version` prints: "Suspense 0.1.42 (owner/name)" for
/// an edge build, or "Suspense 0.1.0 (development build)" for one with no
/// repository.
pub fn describe() -> String {
    match crate::self_update::REPOSITORY {
        Some(repository) => format!("Suspense {VERSION} ({repository})"),
        None => format!("Suspense {VERSION} (development build)"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_describes_the_build() {
        let described = super::describe();
        assert!(described.starts_with(&format!("Suspense {} (", super::VERSION)));
        assert!(described.ends_with(')'));
    }

    #[test]
    fn the_version_is_baked_in() {
        let expected = option_env!("SUSPENSE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
        assert_eq!(super::VERSION, expected);
        assert!(super::VERSION.starts_with("0.1."));
    }
}
