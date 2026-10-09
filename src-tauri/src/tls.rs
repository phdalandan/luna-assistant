use std::sync::{Arc, Once};

use rustls::ClientConfig;
use rustls_platform_verifier::ConfigVerifierExt;

/// Installs `ring` as the process-wide rustls provider. Safe to call repeatedly.
pub fn install_crypto_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        if rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
        {
            log::warn!("a rustls crypto provider was already installed");
        }
    });
}

/// TLS settings that trust the operating system's certificate store.
pub fn client_config() -> Result<Arc<ClientConfig>, rustls::Error> {
    install_crypto_provider();
    Ok(Arc::new(ClientConfig::with_platform_verifier()?))
}
