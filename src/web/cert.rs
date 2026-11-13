//! The certificate the interface serves HTTPS with.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use x509_parser::prelude::{FromDer, GeneralName, X509Certificate};

const CERT_FILE: &str = "web-cert.pem";
const KEY_FILE: &str = "web-key.pem";
const LIFETIME_DAYS: i64 = 800;
const RENEW_BEFORE_DAYS: i64 = 30;
const BACKDATE: Duration = Duration::HOUR;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("generate certificate: {0}")]
    Generate(#[from] rcgen::Error),
    #[error("{0}: no certificate found")]
    NoCertificate(PathBuf),
}

/// The certificate and key the configuration says to serve with, making a
/// self-signed pair beside the state file when asked to; `None` when the
/// interface speaks plain HTTP.
///
/// # Errors
/// When a self-signed pair can't be made.
pub fn files(
    cfg: &crate::config::Config,
    now: Timestamp,
) -> Result<Option<(PathBuf, PathBuf)>, Error> {
    if cfg.web.self_signed {
        let dir = cfg.state.parent().unwrap_or_else(|| Path::new("."));
        return self_signed(dir, &cfg.web.rpid(), now).map(Some);
    }
    if cfg.web.tls_cert.is_empty() {
        return Ok(None);
    }
    Ok(Some((
        PathBuf::from(&cfg.web.tls_cert),
        PathBuf::from(&cfg.web.tls_key),
    )))
}

/// A self-signed certificate for `host` in `dir`, made if there is none that
/// still covers the host and has a month left.
///
/// # Errors
/// When the files can't be read or written, or a certificate can't be made.
pub fn self_signed(dir: &Path, host: &str, now: Timestamp) -> Result<(PathBuf, PathBuf), Error> {
    let (cert_file, key_file) = (dir.join(CERT_FILE), dir.join(KEY_FILE));
    if usable(&cert_file, &key_file, host, now) {
        return Ok((cert_file, key_file));
    }
    std::fs::create_dir_all(dir).map_err(|err| Error::Io(dir.to_path_buf(), err))?;
    let (cert_pem, key_pem) = generate(host, now)?;
    write(&key_file, &key_pem, 0o600)?;
    write(&cert_file, &cert_pem, 0o644)?;
    Ok((cert_file, key_file))
}

fn write(path: &Path, content: &str, mode: u32) -> Result<(), Error> {
    use std::io::Write as _;

    let io = |err| Error::Io(path.to_path_buf(), err);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    let mut file = options.open(path).map_err(io)?;
    // `mode` only applies to a new file; tighten an old one before it gets the key.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(io)?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    file.set_len(0).map_err(io)?;
    file.write_all(content.as_bytes()).map_err(io)
}

fn certificate_der(cert_file: &Path) -> Result<CertificateDer<'static>, Error> {
    CertificateDer::pem_file_iter(cert_file)
        .ok()
        .and_then(|mut certs| certs.next())
        .and_then(Result::ok)
        .ok_or_else(|| Error::NoCertificate(cert_file.to_path_buf()))
}

fn usable(cert_file: &Path, key_file: &Path, host: &str, now: Timestamp) -> bool {
    if !key_file.exists() {
        return false;
    }
    let Ok(der) = certificate_der(cert_file) else {
        return false;
    };
    let Ok((_, cert)) = X509Certificate::from_der(&der) else {
        return false;
    };
    let validity = cert.validity();
    let (start, end) = (
        validity.not_before.timestamp(),
        validity.not_after.timestamp(),
    );
    let renew = RENEW_BEFORE_DAYS * 24 * 60 * 60;
    let names = cert
        .subject_alternative_name()
        .ok()
        .flatten()
        .map_or_default(|san| {
            san.value
                .general_names
                .iter()
                .map(name_of)
                .collect::<Vec<_>>()
        });
    names.iter().any(|name| name.eq_ignore_ascii_case(host))
        && now.as_second() > start
        && now.as_second() + renew < end
}

fn name_of(name: &GeneralName<'_>) -> String {
    match name {
        GeneralName::DNSName(dns) => (*dns).to_owned(),
        GeneralName::IPAddress(bytes) => match bytes.len() {
            4 => std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string(),
            16 => <[u8; 16]>::try_from(*bytes)
                .map_or_default(|b| std::net::Ipv6Addr::from(b).to_string()),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn generate(host: &str, now: Timestamp) -> Result<(String, String), Error> {
    let mut names = vec![host.to_owned()];
    if host != "localhost" {
        names.push("localhost".to_owned());
    }
    let mut params = CertificateParams::new(names)?;
    let mut subject = DistinguishedName::new();
    subject.push(DnType::CommonName, host);
    subject.push(DnType::OrganizationName, "tamis");
    params.distinguished_name = subject;
    let start = OffsetDateTime::from_unix_timestamp(now.as_second())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        - BACKDATE;
    params.not_before = start;
    params.not_after = start + Duration::days(LIFETIME_DAYS);
    params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// The certificate's SHA-256 fingerprint as browsers show it, `AA:BB:…`.
///
/// # Errors
/// When the file can't be read or holds no certificate.
pub fn fingerprint(cert_file: &Path) -> Result<String, Error> {
    if !cert_file.exists() {
        return Err(Error::Io(
            cert_file.to_path_buf(),
            std::io::ErrorKind::NotFound.into(),
        ));
    }
    let der = certificate_der(cert_file)?;
    let digest = Sha256::digest(der.as_ref());
    Ok(digest
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":"))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    #[test]
    fn a_certificate_is_made_for_the_host_with_a_private_key() {
        let dir = tempfile::tempdir().unwrap();

        let (cert, key) = self_signed(dir.path(), "tamis.example.org", at(NOW)).unwrap();

        assert!(usable(&cert, &key, "tamis.example.org", at(NOW)));
        assert!(usable(&cert, &key, "localhost", at(NOW)));
        assert!(!usable(&cert, &key, "other.example.org", at(NOW)));
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fingerprint(&cert).unwrap().split(':').count(), 32);
    }

    #[test]
    fn a_key_file_that_already_exists_is_made_private() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join(KEY_FILE);
        std::fs::write(&key, "old").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();

        self_signed(dir.path(), "tamis.example.org", at(NOW)).unwrap();

        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_usable_certificate_is_kept_and_an_expiring_one_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, _) = self_signed(dir.path(), "tamis.example.org", at(NOW)).unwrap();
        let first = fingerprint(&cert).unwrap();

        self_signed(dir.path(), "tamis.example.org", at(NOW + 86_400)).unwrap();
        let kept = fingerprint(&cert).unwrap();
        self_signed(dir.path(), "tamis.example.org", at(NOW + 790 * 86_400)).unwrap();
        let renewed = fingerprint(&cert).unwrap();
        self_signed(dir.path(), "new.example.org", at(NOW + 790 * 86_400)).unwrap();

        assert_eq!(first, kept);
        assert_ne!(first, renewed);
        assert!(usable(
            &cert,
            &dir.path().join(KEY_FILE),
            "new.example.org",
            at(NOW + 790 * 86_400)
        ));
    }

    #[test]
    fn a_file_without_a_certificate_has_no_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.pem");
        std::fs::write(&file, "not a certificate").unwrap();

        assert!(matches!(fingerprint(&file), Err(Error::NoCertificate(_))));
        assert!(matches!(
            fingerprint(&dir.path().join("missing.pem")),
            Err(Error::Io(..))
        ));
    }
}
