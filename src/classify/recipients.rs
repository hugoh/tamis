use super::Model;

/// Caps the `to:` tokens of one message, so a mail to a big group doesn't
/// flood the model.
pub(super) const MAX_RECIPIENTS: usize = 10;
/// How many good messages an address must have been a recipient of to count
/// as one mail is expected on.
const MIN_SEEN_RECIPIENT: u64 = 3;

pub(super) const RECIPIENT_PREFIX: &str = "to:";

const STATUS_MINE: &str = "rcpt:mine";
const STATUS_OTHER: &str = "rcpt:other";
const STATUS_NONE: &str = "rcpt:none";

/// Every token [`with_recipient_status`] can add, so a model can be asked
/// about them before the status is known.
#[must_use]
pub fn recipient_status_tokens() -> [&'static str; 3] {
    [STATUS_MINE, STATUS_OTHER, STATUS_NONE]
}

/// Returns `tokens` plus one `rcpt:` token saying whether the message went to
/// an address mail is expected on: one of `own`, or one that at least three
/// good messages were sent to. Spam doesn't teach what to expect, so a
/// spammer's target address stays unexpected.
///
/// shortcut: counts since the model began, with no memory of when; an alias
/// you retire stays expected. Upgrade by keeping recent counts.
#[must_use]
pub fn with_recipient_status(
    tokens: &[String],
    model: &(impl Model + ?Sized),
    own: &[String],
) -> Vec<String> {
    let mut status = STATUS_NONE;

    for address in tokens
        .iter()
        .filter_map(|token| token.strip_prefix(RECIPIENT_PREFIX))
    {
        status = STATUS_OTHER;
        if expected(address, model, own) {
            status = STATUS_MINE;
            break;
        }
    }

    let mut out = tokens.to_vec();
    out.push(status.to_owned());
    out
}

fn expected(address: &str, model: &(impl Model + ?Sized), own: &[String]) -> bool {
    own.iter().any(|mine| mine.eq_ignore_ascii_case(address))
        || model.counts(&format!("{RECIPIENT_PREFIX}{address}")).1 >= MIN_SEEN_RECIPIENT
}
