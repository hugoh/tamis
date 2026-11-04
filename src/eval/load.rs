//! Reads `ham/` and `spam/` folders of messages, oldest first, and mixes them
//! so each class keeps its share over time.

use std::path::Path;

use jiff::Timestamp;
use mail_parser::MessageParser;

use super::{Error, Message};
use crate::mail::MAX_MESSAGE_BYTES;

struct Dated {
    at: i64,
    raw: Vec<u8>,
}

/// # Errors
/// When either folder is missing or empty, or a file can't be read.
pub fn load(root: &Path) -> Result<Vec<Message>, Error> {
    let ham = read_folder(&root.join("ham"))?;
    let spam = read_folder(&root.join("spam"))?;
    Ok(interleave(ham, spam))
}

fn read_folder(dir: &Path) -> Result<Vec<Dated>, Error> {
    let mut found = Vec::new();
    collect(dir, &mut found)?;
    if found.is_empty() {
        return Err(Error::Io(format!(
            "load {}: {}",
            dir.display(),
            Error::NoMessages
        )));
    }
    found.sort_by_key(|message| message.at);
    Ok(found)
}

fn collect(dir: &Path, found: &mut Vec<Dated>) -> Result<(), Error> {
    let io = |what: &str, path: &Path, err: std::io::Error| {
        Error::Io(format!("{what} {}: {err}", path.display()))
    };
    let entries = std::fs::read_dir(dir).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::Io(format!("load {}: {}", dir.display(), Error::NoMessages))
        } else {
            io("load", dir, err)
        }
    })?;
    let mut paths: Vec<_> = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| io("read", dir, err))?;
    paths.sort_by_key(std::fs::DirEntry::path);
    for entry in paths {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, found)?;
            continue;
        }
        let mut raw = std::fs::read(&path).map_err(|err| io("read", &path, err))?;
        raw.truncate(MAX_MESSAGE_BYTES);
        found.push(Dated {
            at: sent(&raw),
            raw,
        });
    }
    Ok(())
}

/// When the message says it was sent, or the earliest time when it doesn't.
fn sent(raw: &[u8]) -> i64 {
    MessageParser::default()
        .parse(raw)
        .and_then(|message| message.date().map(mail_parser::DateTime::to_timestamp))
        .and_then(|seconds| Timestamp::from_second(seconds).ok())
        .map_or(i64::MIN, Timestamp::as_second)
}

/// Spreads the spam evenly among the good mail.
fn interleave(ham: Vec<Dated>, spam: Vec<Dated>) -> Vec<Message> {
    let (ham_count, spam_count) = (ham.len(), spam.len());
    let mut spam = spam.into_iter().enumerate().peekable();
    let mut out = Vec::with_capacity(ham_count + spam_count);
    for (index, message) in ham.into_iter().enumerate() {
        out.push(Message {
            raw: message.raw,
            spam: false,
        });
        while let Some((next, _)) = spam.peek() {
            if next * ham_count > (index + 1) * spam_count {
                break;
            }
            let Some((_, message)) = spam.next() else {
                break;
            };
            out.push(Message {
                raw: message.raw,
                spam: true,
            });
        }
    }
    out.extend(spam.map(|(_, message)| Message {
        raw: message.raw,
        spam: true,
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dated(at: i64) -> Dated {
        Dated {
            at,
            raw: at.to_string().into_bytes(),
        }
    }

    #[test]
    fn spam_is_spread_among_the_good_mail() {
        let order: Vec<(String, bool)> =
            interleave((1..=4).map(dated).collect(), (11..=12).map(dated).collect())
                .into_iter()
                .map(|m| (String::from_utf8(m.raw).unwrap(), m.spam))
                .collect();

        let spam_at: Vec<usize> = order
            .iter()
            .enumerate()
            .filter(|(_, (_, spam))| *spam)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(order.len(), 6);
        assert_eq!(spam_at, [1, 3]);
    }

    #[test]
    fn leftover_spam_goes_last() {
        let order = interleave(vec![dated(1)], (11..=13).map(dated).collect());

        assert_eq!(order.iter().filter(|m| m.spam).count(), 3);
        assert!(!order[0].spam);
    }

    #[test]
    fn folders_load_oldest_first_and_recurse() {
        let dir = tempfile::tempdir().unwrap();
        for (class, files) in [
            (
                "ham",
                [
                    ("b.eml", "Tue, 02 Jan 2024 10:00:00 +0000"),
                    ("a.eml", "Mon, 01 Jan 2024 10:00:00 +0000"),
                ],
            ),
            (
                "spam",
                [
                    ("s.eml", "Mon, 01 Jan 2024 10:00:00 +0000"),
                    ("t.eml", "Tue, 02 Jan 2024 10:00:00 +0000"),
                ],
            ),
        ] {
            std::fs::create_dir_all(dir.path().join(class).join("sub")).unwrap();
            for (name, date) in files {
                std::fs::write(
                    dir.path().join(class).join("sub").join(name),
                    format!("Date: {date}\r\nSubject: {name}\r\n\r\nbody"),
                )
                .unwrap();
            }
        }

        let messages = load(dir.path()).unwrap();

        let subjects: Vec<String> = messages
            .iter()
            .map(|m| {
                String::from_utf8_lossy(&m.raw)
                    .lines()
                    .nth(1)
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            subjects,
            [
                "Subject: a.eml",
                "Subject: s.eml",
                "Subject: t.eml",
                "Subject: b.eml",
            ]
        );
    }

    #[test]
    fn a_missing_or_empty_folder_is_no_messages() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("ham")).unwrap();

        let err = load(dir.path()).unwrap_err().to_string();

        assert!(err.contains("ham") && err.contains("no messages"), "{err}");
    }
}
