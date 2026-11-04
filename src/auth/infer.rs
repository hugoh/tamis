use std::collections::HashMap;

use super::header;
use super::results::parse;

/// How many messages it takes to tell a provider from noise.
const MIN_SAMPLE: usize = 20;
/// The provider's domain must head nine in ten messages.
const SHARE: (usize, usize) = (9, 10);

/// Counts, over a sample of messages, the registered domain of the authserv-id
/// on each message's topmost `Authentication-Results` header. The provider
/// prepends its own header to everything it delivers, so its domain heads
/// nearly all of them; a header a sender forged heads only the few it was
/// written for.
#[derive(Debug, Default)]
pub struct Tally {
    total: usize,
    domains: HashMap<String, usize>,
}

impl Tally {
    /// Counts one message. Messages with no usable header still count towards
    /// the total, which keeps a sample full of unauthenticated mail from
    /// naming a provider.
    pub fn add(&mut self, raw: &[u8]) {
        self.total += 1;
        if let Some(domain) = top_domain(raw) {
            *self.domains.entry(domain).or_default() += 1;
        }
    }

    /// How many messages were counted.
    #[must_use]
    pub fn total(&self) -> usize {
        self.total
    }

    /// The provider's domain, if one heads enough of the sample.
    #[must_use]
    pub fn infer(&self) -> Option<String> {
        if self.total < MIN_SAMPLE {
            return None;
        }
        self.domains
            .iter()
            .find(|(_, count)| **count * SHARE.1 >= self.total * SHARE.0)
            .map(|(domain, _)| domain.clone())
    }
}

fn top_domain(raw: &[u8]) -> Option<String> {
    let value = header::values(raw, "Authentication-Results")
        .into_iter()
        .next()?;
    let parsed = parse(&header::strip_comments(&value))?;
    psl::domain_str(&parsed.authserv_id.to_lowercase()).map(str::to_owned)
}
