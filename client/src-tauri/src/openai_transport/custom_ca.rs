//! Shared HTTPS/WSS trust policy. Read once per process, like startup environment
//! configuration; restart CONST after changing a CA bundle. Failed loads are not
//! cached, and never fall back to system-only trust after an explicit override.

use anyhow::{Context, Result, bail};
use rustls_pki_types::{CertificateDer, pem::PemObject};
use std::{
    ffi::OsString,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

pub(crate) fn tls_config() -> Result<Arc<rustls::ClientConfig>> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config.clone());
    }
    let bundle = selected_bundle(
        std::env::var_os("CODEX_CA_CERTIFICATE"),
        std::env::var_os("SSL_CERT_FILE"),
    );
    let config = Arc::new(build_config(bundle)?);
    Ok(CONFIG.get_or_init(|| config).clone())
}

fn selected_bundle(
    codex: Option<OsString>,
    ssl: Option<OsString>,
) -> Option<(&'static str, PathBuf)> {
    codex
        .filter(|path| !path.is_empty())
        .map(|path| ("CODEX_CA_CERTIFICATE", PathBuf::from(path)))
        .or_else(|| {
            ssl.filter(|path| !path.is_empty())
                .map(|path| ("SSL_CERT_FILE", PathBuf::from(path)))
        })
}

fn build_config(bundle: Option<(&str, PathBuf)>) -> Result<rustls::ClientConfig> {
    let roots = match bundle {
        None => Vec::new(),
        Some((source, path)) => {
            let hint = format!(
                "invalid OpenAI subscription CA bundle from {source} ({}); configure a readable PEM certificate bundle and restart CONST",
                path.display()
            );
            let data = std::fs::read(&path).with_context(|| hint.clone())?;
            parse_bundle(&data).with_context(|| hint)?
        }
    };
    // Codex's standard dependency graph enables prefer-post-quantum. Match its
    // order here, without changing the global provider used by other adapters.
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    provider
        .kx_groups
        .sort_by_key(|group| group.name() != rustls::NamedGroup::X25519MLKEM768);
    let provider = Arc::new(provider);
    let native = rustls_native_certs::load_native_certs();
    if !native.errors.is_empty() {
        log::warn!(
            "[const-api][codex] {} native CA entries could not be loaded",
            native.errors.len()
        );
    }
    let mut store = rustls::RootCertStore::empty();
    store.add_parsable_certificates(native.certs);
    for root in roots {
        store
            .add(root)
            .context("invalid OpenAI subscription CA trust anchor")?;
    }
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(store)
        .with_no_client_auth())
}

fn parse_bundle(data: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    // OpenSSL exports TRUSTED CERTIFICATE blocks with optional trailing X509_AUX
    // metadata. Match Codex's normalization; valid CRLs/keys are ignored by the
    // certificate iterator. Never include PEM contents in diagnostics.
    let text = std::str::from_utf8(data).context("CA bundle must be PEM text")?;
    let normalized = text
        .replace(
            "-----BEGIN TRUSTED CERTIFICATE-----",
            "-----BEGIN CERTIFICATE-----",
        )
        .replace(
            "-----END TRUSTED CERTIFICATE-----",
            "-----END CERTIFICATE-----",
        );
    let mut roots = Vec::new();
    for result in CertificateDer::pem_slice_iter(normalized.as_bytes()) {
        let der = result.map_err(|_| anyhow::anyhow!("malformed CA PEM certificate"))?;
        let (_, cert) = x509_parser::parse_x509_certificate(der.as_ref())
            .map_err(|_| anyhow::anyhow!("invalid CA X.509 certificate"))?;
        let cert = CertificateDer::from(cert.as_raw().to_vec());
        // Validate all roots at the boundary, including on platforms whose
        // verifier may defer parsing until the first handshake.
        rustls::RootCertStore::empty()
            .add(cert.clone())
            .context("invalid CA trust anchor")?;
        roots.push(cert);
    }
    if roots.is_empty() {
        bail!("no certificates found in CA PEM bundle");
    }
    Ok(roots)
}

#[cfg(test)]
mod tests;
