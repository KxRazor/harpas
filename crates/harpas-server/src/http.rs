//! HTTPS requests to Mojang: rustls with the system's root certificates, through `HTTPS_PROXY` when it is set

use std::sync::Arc;

use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};

/// TLS that trusts the system's root certificates.
pub fn tls() -> Result<TlsConfig, String> {
    let native = rustls_native_certs::load_native_certs();
    if native.certs.is_empty() {
        return Err(format!(
            "no root certificates found on this system: {:?}",
            native.errors
        ));
    }
    let roots: Vec<Certificate<'static>> = native
        .certs
        .iter()
        .map(|cert| Certificate::from_der(cert.as_ref()).to_owned())
        .collect();
    Ok(TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::new_with_certs(&roots))
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build())
}
