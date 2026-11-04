pub mod auth;
pub mod classify;
pub mod config;
pub mod report;
pub mod retry;
pub mod secret;

#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn version_is_semver_shaped() {
        assert_eq!(version().split('.').count(), 3);
    }
}
