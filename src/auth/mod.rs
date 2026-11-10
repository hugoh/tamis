//! Decides whether a message's From address is authentic, according to the
//! receiving provider's `Authentication-Results` header (RFC 8601).

mod header;
mod infer;
mod results;

use std::collections::HashSet;

use mail_parser::MessageParser;

pub use infer::Tally;
use results::{Method, parse};

/// Reports whether the provider recorded a DMARC pass, or a DKIM pass aligned
/// with the From domain.
///
/// Without `server_id` only the topmost `Authentication-Results` header
/// counts: the provider prepends its own, and anything below it may have been
/// written by the sender. Some providers (iCloud) prepend several, one per
/// check, each with its own authserv-id such as `dmarc.icloud.com`;
/// `server_id` then names the provider's domain ("icloud.com") and the
/// leading headers whose authserv-id is that domain or a subdomain of it
/// count, up to the first one that is not. Only the first header per
/// authserv-id counts, so a forged copy of one of the provider's ids below the
/// genuine one is ignored. A forged header with an id the provider never
/// uses, directly below its block, would still be believed.
#[must_use]
pub fn verified(raw: &[u8], server_id: &str) -> bool {
    let Some(domain) = from_domain(raw) else {
        return false;
    };

    let mut seen = HashSet::new();
    for value in header::values(raw, "Authentication-Results") {
        let Some(parsed) = parse(&header::strip_comments(&value)) else {
            return false;
        };
        if !from_provider(&parsed.authserv_id, server_id) {
            return false;
        }
        if !seen.insert(parsed.authserv_id.to_lowercase()) {
            continue;
        }
        if parsed
            .results
            .iter()
            .any(|result| passes_for(result, &domain))
        {
            return true;
        }
        if server_id.is_empty() {
            return false;
        }
    }

    false
}

/// A name that senders impersonate, and the domains that may really use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brand {
    pub name: String,
    pub domains: Vec<String>,
}

/// Returns the brand whose name the From display name claims while the From
/// address is on none of the brand's domains or their subdomains.
///
/// Authentication can't catch this: the impersonator authenticates its own
/// domain, usually through a real sending service.
#[must_use]
pub fn impersonated<'a>(raw: &[u8], brands: &'a [Brand]) -> Option<&'a Brand> {
    let message = MessageParser::default().parse_headers(raw)?;
    let from = message.from()?.first()?;
    let name = from.name()?.to_lowercase();
    let domain = from_domain(raw)?;
    brands.iter().find(|brand| {
        name.contains(&brand.name.to_lowercase())
            && !brand
                .domains
                .iter()
                .any(|own| aligned(&own.to_lowercase(), &domain))
    })
}

/// Reports whether `authserv_id` is the configured provider's: its domain or a
/// subdomain. Any id will do when none is configured.
fn from_provider(authserv_id: &str, server_id: &str) -> bool {
    if server_id.is_empty() {
        return true;
    }
    let (id, server) = (authserv_id.to_lowercase(), server_id.to_lowercase());
    id == server || id.ends_with(&format!(".{server}"))
}

fn from_domain(raw: &[u8]) -> Option<String> {
    let message = MessageParser::default().parse_headers(raw)?;
    let address = message.from()?.first()?.address()?;
    address
        .to_lowercase()
        .split_once('@')
        .map(|(_, domain)| domain.to_owned())
}

fn passes_for(result: &results::Result, domain: &str) -> bool {
    if !result.value.eq_ignore_ascii_case("pass") {
        return false;
    }
    match result.method {
        Method::Dmarc => {
            let from = result
                .property("header.from")
                .unwrap_or_default()
                .to_lowercase();
            from.is_empty() || from == domain
        }
        Method::Dkim => aligned(
            &result
                .property("header.d")
                .unwrap_or_default()
                .to_lowercase(),
            domain,
        ),
        Method::Other => false,
    }
}

/// DMARC's relaxed alignment: the signing domain is the From domain or one of
/// its parents.
fn aligned(signer: &str, from: &str) -> bool {
    !signer.is_empty() && (from == signer || from.ends_with(&format!(".{signer}")))
}
