use serde::Deserialize;
use url::{Host, Url};

use super::{is_loopback, split_host_port};

/// The optional web interface, served by `tamis run`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Web {
    pub enabled: bool,
    /// The address to bind. Anything but loopback needs an https origin: the
    /// server speaks plain HTTP and expects a TLS-terminating proxy.
    pub listen: String,
    /// The URL browsers reach the interface at. Its host is the passkeys'
    /// relying party ID, so changing it invalidates enrolled passkeys.
    pub origin: String,
    /// PEM files for serving HTTPS directly. Both or neither.
    pub tls_cert: String,
    pub tls_key: String,
    /// Serves HTTPS with a certificate tamis makes for the origin's host.
    pub self_signed: bool,
}

impl Default for Web {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: "127.0.0.1:8484".to_owned(),
            origin: String::new(),
            tls_cert: String::new(),
            tls_key: String::new(),
            self_signed: false,
        }
    }
}

impl Web {
    /// Whether the server speaks HTTPS itself.
    #[must_use]
    pub fn tls(&self) -> bool {
        self.self_signed || !self.tls_cert.is_empty()
    }

    /// The `WebAuthn` relying party ID: the host of the origin.
    #[must_use]
    pub fn rpid(&self) -> String {
        match Url::parse(&self.origin)
            .ok()
            .and_then(|url| url.host().map(|h| h.to_owned()))
        {
            Some(Host::Domain(domain)) => domain,
            Some(Host::Ipv4(ip)) => ip.to_string(),
            Some(Host::Ipv6(ip)) => ip.to_string(),
            None => String::new(),
        }
    }

    pub(super) fn problems(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }

        let mut problems = Vec::new();
        let origin = self.parsed_origin();
        if origin.is_none() {
            problems.push(
                "web.origin must be an http(s) URL without a path, e.g. https://tamis.example.net"
                    .to_owned(),
            );
        }

        let Some(host) = split_host_port(&self.listen) else {
            problems.push("web.listen must be host:port, e.g. 127.0.0.1:8484".to_owned());
            return problems;
        };

        if origin.as_ref().is_some_and(|o| o.scheme() != "https") && !is_loopback(host) {
            problems.push(
                "web.listen is not loopback, so web.origin must be https (put a TLS proxy in front or set self_signed)"
                    .to_owned(),
            );
        }

        problems.extend(self.tls_problems(origin.as_ref()));
        problems
    }

    fn tls_problems(&self, origin: Option<&Url>) -> Vec<String> {
        let mut problems = Vec::new();
        if self.tls_cert.is_empty() != self.tls_key.is_empty() {
            problems
                .push("web.tls_cert and web.tls_key go together: set both or neither".to_owned());
        }
        if self.self_signed && !self.tls_cert.is_empty() {
            problems.push("web.self_signed and web.tls_cert are alternatives: set one".to_owned());
        }
        if self.tls() && origin.is_some_and(|o| o.scheme() != "https") {
            problems.push("web.origin must be https when tamis serves TLS".to_owned());
        }
        problems
    }

    fn parsed_origin(&self) -> Option<Url> {
        let origin = Url::parse(&self.origin).ok()?;
        let plain_path = matches!(origin.path(), "" | "/");
        (origin.host_str().is_some() && plain_path && matches!(origin.scheme(), "http" | "https"))
            .then_some(origin)
    }
}
