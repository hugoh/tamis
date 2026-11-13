//! Conversions between IMAP protocol types and the types the rest of tamis
//! uses.

use imap_client::imap_types::IntoStatic;
use imap_client::imap_types::datetime::NaiveDate;
use imap_client::imap_types::envelope::{Address, Envelope};
use imap_client::imap_types::fetch::MessageDataItem;
use imap_client::imap_types::flag::{Flag, FlagFetch};
use imap_client::imap_types::mailbox::Mailbox as ImapMailbox;
use imap_client::imap_types::sequence::SequenceSet;
use jiff::Timestamp;
use mail_parser::MessageParser;

use super::utf7;
use super::{Error, Message, Result};

pub(super) fn invalid<E: std::fmt::Display>(err: E) -> Error {
    Error::Server(format!("invalid argument: {err}"))
}

pub(super) fn uid_set(uids: &[u32]) -> Result<SequenceSet> {
    let list = uids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    SequenceSet::try_from(list.as_str()).map_err(invalid)
}

pub(super) fn text(value: &imap_client::imap_types::core::NString<'_>) -> String {
    value
        .0
        .as_ref()
        .map_or_default(|s| String::from_utf8_lossy(s.as_ref()).into_owned())
}

/// Normalises a Message-ID to its `<...>` form.
pub(super) fn bracket(message_id: &str) -> String {
    if message_id.is_empty() || message_id.starts_with('<') {
        message_id.to_owned()
    } else {
        format!("<{message_id}>")
    }
}

pub(super) fn address_of(address: &Address<'_>) -> String {
    let (mailbox, host) = (text(&address.mailbox), text(&address.host));
    if mailbox.is_empty() || host.is_empty() {
        String::new()
    } else {
        format!("{mailbox}@{host}").to_lowercase()
    }
}

/// The subject with RFC 2047 encoded words decoded.
pub(super) fn decode_subject(raw: &str) -> String {
    if !raw.contains("=?") {
        return raw.to_owned();
    }
    MessageParser::default()
        .parse(format!("Subject: {raw}\r\n\r\n").as_bytes())
        .and_then(|m| m.subject().map(str::to_owned))
        .unwrap_or_else(|| raw.to_owned())
}

pub(super) fn to_message(uid: u32, items: &[MessageDataItem<'static>]) -> Message {
    let mut message = Message {
        uid,
        ..Message::default()
    };
    for item in items {
        match item {
            MessageDataItem::Flags(flags) => {
                message.flags = flags
                    .iter()
                    .map(|flag| flag_name(flag).to_lowercase())
                    .collect();
            }
            MessageDataItem::BodyExt { data, .. } => {
                message.raw = data.0.as_ref().map_or_default(|d| d.as_ref().to_vec());
            }
            MessageDataItem::Envelope(envelope) => fill_from_envelope(&mut message, envelope),
            _ => {}
        }
    }
    message
}

pub(super) fn fill_from_envelope(message: &mut Message, envelope: &Envelope<'_>) {
    message.subject = decode_subject(&text(&envelope.subject));
    message.message_id = bracket(&text(&envelope.message_id));
    message.from = envelope.from.first().map_or_default(address_of);
}

pub(super) fn flag_name(flag: &FlagFetch<'_>) -> String {
    match flag {
        FlagFetch::Flag(flag) => flag.to_string(),
        FlagFetch::Recent => "\\Recent".to_owned(),
    }
}

pub(super) fn date(timestamp: Timestamp) -> NaiveDate {
    let day = chrono::DateTime::from_timestamp(timestamp.as_second(), 0)
        .map_or(chrono::NaiveDate::MIN, |at| at.date_naive());
    NaiveDate::unvalidated(day)
}

pub(super) fn keyword_flag(keyword: &str) -> Result<Flag<'static>> {
    Ok(Flag::try_from(keyword).map_err(invalid)?.into_static())
}

pub(super) fn mailbox_name(mailbox: &ImapMailbox<'_>) -> String {
    match mailbox {
        ImapMailbox::Inbox => "INBOX".to_owned(),
        ImapMailbox::Other(other) => utf7::decode(&String::from_utf8_lossy(other.inner().as_ref())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_ids_get_their_brackets() {
        assert_eq!(bracket("a@b"), "<a@b>");
        assert_eq!(bracket("<a@b>"), "<a@b>");
        assert_eq!(bracket(""), "");
    }

    #[test]
    fn subjects_are_decoded_only_when_encoded() {
        assert_eq!(decode_subject("plain"), "plain");
        assert_eq!(decode_subject("=?UTF-8?B?w6l0w6k=?="), "été");
        assert_eq!(decode_subject("=?broken"), "=?broken");
    }

    #[test]
    fn uid_sets_join_the_uids_and_refuse_none() {
        assert_eq!(
            uid_set(&[3, 7]).unwrap(),
            SequenceSet::try_from("3,7").unwrap()
        );
        assert!(uid_set(&[]).is_err());
    }

    #[test]
    fn dates_are_the_utc_day() {
        let at = Timestamp::from_second(86_400 * 365).unwrap();
        assert_eq!(date(at).as_ref().to_string(), "1971-01-01");
    }

    #[test]
    fn the_inbox_keeps_its_name_and_others_are_decoded() {
        assert_eq!(mailbox_name(&ImapMailbox::Inbox), "INBOX");
        let other = ImapMailbox::try_from("Z&AOk-").unwrap();
        assert_eq!(mailbox_name(&other), "Zé");
    }

    #[test]
    fn keywords_that_are_not_atoms_are_refused() {
        assert!(keyword_flag("$TamisFiled").is_ok());
        assert!(keyword_flag("two words").is_err());
    }
}
