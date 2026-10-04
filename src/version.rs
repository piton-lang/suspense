//! The application's own version, baked in when it is built, as the
//! EdgeReleasesScope says: an edge release's `0.1.N`, given by its workflow
//! as `SUSPENSE_VERSION`, and otherwise the version Cargo.toml gives.

/// The application's version.
pub const VERSION: &str = match option_env!("SUSPENSE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

#[cfg(test)]
mod tests {
    #[test]
    fn the_version_is_baked_in() {
        let expected = option_env!("SUSPENSE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
        assert_eq!(super::VERSION, expected);
        assert!(super::VERSION.starts_with("0.1."));
    }
}
