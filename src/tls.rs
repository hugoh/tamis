//! The TLS crypto provider. `ring` is the only one compiled in, and reqwest is
//! built without a provider of its own, so tamis installs it once, before any
//! connection is made.

use std::sync::Once;

/// Installs `ring` as the process-wide provider. Safe to call repeatedly, and
/// when something else already installed one.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_provider_is_installed_and_installing_again_is_harmless() {
        super::init();
        super::init();

        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
