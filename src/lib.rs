pub mod auth;
pub mod classify;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod engine;
pub mod eval;
pub mod health;
pub mod live;
pub mod mail;
pub mod monitor;
pub mod probe;
pub mod report;
pub mod retry;
pub mod rules;
pub mod secret;
pub mod store;
pub mod tls;
pub mod web;

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
