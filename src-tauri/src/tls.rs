//! TLS configuration. SQLighter never offers "encrypt without verification": every TLS mode
//! authenticates the server, otherwise a man-in-the-middle could transparently intercept traffic.
//!
//! * verify-full: chain against system + Mozilla roots (+ optional custom CA) AND host name.
//! * verify-ca:   chain is verified, host name mismatch is tolerated.
//! * pinned:      exactly one certificate (SHA-256 of the DER) is accepted. The TLS handshake
//!                signature is still verified, so only the holder of the private key can connect.

use std::net::IpAddr;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::model::{CertInfo, ConnectionConfig, TlsConfig, TlsMode};

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

pub fn install_default_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub fn fingerprint(der: &[u8]) -> String {
    let d = Sha256::digest(der);
    d.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

fn normalize_fp(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_ascii_uppercase()
}

pub fn is_loopback(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if h.eq_ignore_ascii_case("localhost") || h.is_empty() || h.starts_with('/') {
        return true;
    }
    h.parse::<IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// Refuses plain-text connections to remote hosts unless explicitly acknowledged by the user.
pub fn ensure_transport_allowed(cfg: &ConnectionConfig) -> Result<()> {
    if cfg.tls.mode == TlsMode::Disabled && !cfg.ssh.enabled && !is_loopback(&cfg.host) && !cfg.tls.allow_insecure {
        bail!(
            "TLS is disabled for the remote host '{}'. Credentials and data would travel unencrypted and could be \
             intercepted (MITM). Enable TLS, use an SSH tunnel, or explicitly acknowledge the risk in the connection settings.",
            cfg.host
        );
    }
    Ok(())
}

pub fn load_certs_pem(path: &str) -> Result<Vec<CertificateDer<'static>>> {
    let certs: Vec<_> = CertificateDer::pem_file_iter(path)
        .with_context(|| format!("read certificate file {path}"))?
        .collect::<Result<_, _>>()
        .with_context(|| format!("parse certificate file {path}"))?;
    if certs.is_empty() {
        bail!("no PEM certificate found in {path}");
    }
    Ok(certs)
}

pub fn root_store(ca_file: Option<&str>, include_system: bool) -> Result<RootCertStore> {
    let mut roots = RootCertStore::empty();
    if include_system {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let native = rustls_native_certs::load_native_certs();
        for c in native.certs {
            let _ = roots.add(c);
        }
    }
    if let Some(ca) = ca_file.filter(|s| !s.is_empty()) {
        for c in load_certs_pem(ca)? {
            roots.add(c).context("invalid CA certificate")?;
        }
    }
    Ok(roots)
}

/// Builds a rustls client config for the given TLS settings.
pub fn client_config(tls: &TlsConfig) -> Result<Arc<ClientConfig>> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions()?;
    let builder = match tls.mode {
        TlsMode::Disabled => bail!("TLS disabled"),
        TlsMode::VerifyFull => {
            let custom = tls.ca_file.as_deref().filter(|s| !s.is_empty());
            // A custom CA replaces the public roots: the server must chain to *that* CA.
            builder.with_root_certificates(root_store(custom, custom.is_none())?)
        }
        TlsMode::VerifyCa => {
            let custom = tls.ca_file.as_deref().filter(|s| !s.is_empty());
            let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(root_store(custom, custom.is_none())?), provider.clone()).build()?;
            builder.dangerous().with_custom_certificate_verifier(Arc::new(VerifyCaOnly { inner }))
        }
        TlsMode::Pinned => {
            let fp = match (&tls.pinned_fingerprint, &tls.pinned_cert) {
                (Some(fp), _) if !fp.is_empty() => normalize_fp(fp),
                (_, Some(pem)) if !pem.is_empty() => {
                    let der = CertificateDer::from_pem_slice(pem.as_bytes()).context("invalid pinned certificate")?;
                    normalize_fp(&fingerprint(&der))
                }
                _ => bail!("Pinned TLS mode requires a trusted certificate. Use 'Fetch certificate' in the connection dialog."),
            };
            if fp.len() != 64 {
                bail!("invalid pinned SHA-256 fingerprint");
            }
            builder.dangerous().with_custom_certificate_verifier(Arc::new(Pinned { fingerprint: fp, provider: provider.clone() }))
        }
    };
    let cfg = match (&tls.cert_file, &tls.key_file) {
        (Some(c), Some(k)) if !c.is_empty() && !k.is_empty() => {
            let certs = load_certs_pem(c)?;
            let key = PrivateKeyDer::from_pem_file(k).with_context(|| format!("read client key {k}"))?;
            builder.with_client_auth_cert(certs, key)?
        }
        _ => builder.with_no_client_auth(),
    };
    Ok(Arc::new(cfg))
}

#[derive(Debug)]
struct VerifyCaOnly {
    inner: Arc<WebPkiServerVerifier>,
}

impl ServerCertVerifier for VerifyCaOnly {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp, now) {
            Ok(v) => Ok(v),
            // Chain is valid, only the host name does not match: accepted in verify-ca mode.
            Err(rustls::Error::InvalidCertificate(CertificateError::NotValidForName))
            | Err(rustls::Error::InvalidCertificate(CertificateError::NotValidForNameContext { .. })) => Ok(ServerCertVerified::assertion()),
            Err(e) => Err(e),
        }
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(m, c, d)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[derive(Debug)]
struct Pinned {
    fingerprint: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let got = normalize_fp(&fingerprint(end_entity));
        // Constant-time comparison of the fingerprints.
        let eq = got.len() == self.fingerprint.len() && got.bytes().zip(self.fingerprint.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
        if eq {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "SERVER CERTIFICATE CHANGED! Expected fingerprint {} but got {}. This can indicate a man-in-the-middle attack.",
                self.fingerprint, got
            )))
        }
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Verifier used only by the certificate probe: records the certificate and accepts it, but the
/// handshake is aborted right after and no credentials are ever sent over this connection.
#[derive(Debug)]
struct Recorder {
    provider: Arc<CryptoProvider>,
    seen: std::sync::Mutex<Option<Vec<u8>>>,
}

impl ServerCertVerifier for Recorder {
    fn verify_server_cert(&self, ee: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        *self.seen.lock().unwrap() = Some(ee.to_vec());
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

pub fn server_name(host: &str) -> Result<ServerName<'static>> {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']').to_string();
    ServerName::try_from(h).map_err(|e| anyhow::anyhow!("invalid TLS server name: {e}"))
}

/// Fetches the certificate a PostgreSQL server presents (for trust-on-first-use pinning).
pub async fn probe_postgres_cert<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, host: &str, port: u16) -> Result<CertInfo> {
    // SSLRequest: int32 length 8, int32 code 80877103
    stream.write_all(&[0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f]).await?;
    let mut b = [0u8; 1];
    stream.read_exact(&mut b).await?;
    if b[0] != b'S' {
        bail!("the server does not support TLS");
    }
    probe_tls(stream, host, port).await
}

async fn probe_tls<S: AsyncRead + AsyncWrite + Unpin>(stream: S, host: &str, port: u16) -> Result<CertInfo> {
    let provider = provider();
    let rec = Arc::new(Recorder { provider: provider.clone(), seen: Default::default() });
    let cfg = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(rec.clone())
        .with_no_client_auth();
    let conn = tokio_rustls::TlsConnector::from(Arc::new(cfg));
    let name = server_name(host).unwrap_or_else(|_| ServerName::try_from("localhost").unwrap());
    let mut tls = tokio::time::timeout(std::time::Duration::from_secs(15), conn.connect(name, stream)).await.context("TLS handshake timed out")??;
    let _ = tls.shutdown().await;
    let der = rec.seen.lock().unwrap().clone().context("server did not present a certificate")?;
    cert_info(&der, host, port)
}

pub fn cert_info(der: &[u8], host: &str, port: u16) -> Result<CertInfo> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).map_err(|e| anyhow::anyhow!("invalid certificate: {e}"))?;
    let v = cert.validity();
    let pem = {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(der);
        let lines: Vec<&str> = b64.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap()).collect();
        format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", lines.join("\n"))
    };
    Ok(CertInfo {
        host: host.to_string(),
        port,
        fingerprint: fingerprint(der),
        subject: cert.subject().to_string(),
        issuer: cert.issuer().to_string(),
        valid_from: v.not_before.to_string(),
        valid_to: v.not_after.to_string(),
        pem,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("localhost"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("::1"));
        assert!(is_loopback("[::1]"));
        assert!(!is_loopback("db.example.com"));
        assert!(!is_loopback("10.0.0.5"));
    }

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint(b"abc");
        assert_eq!(fp.len(), 32 * 3 - 1);
        assert_eq!(normalize_fp(&fp).len(), 64);
        assert_eq!(normalize_fp("ab:cd"), "ABCD");
    }
}
