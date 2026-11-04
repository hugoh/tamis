//! Files mail into folders by running a Sieve script (RFC 5228) against it,
//! the way a server-side filter would.

mod scan;

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use mail_parser::{Addr, MessageParser};
use sieve::{Compiler, Envelope, Event, Input, Runtime, Sieve};
use thiserror::Error;

/// How far a folder may be from a target and still be offered as its
/// correction.
const MAX_TYPOS: usize = 2;

#[derive(Debug, Error)]
pub enum Error {
    #[error("read rules: {0}")]
    Read(#[from] std::io::Error),
    #[error("parse rules: {0}")]
    Parse(String),
    #[error("run rules: {0}")]
    Run(String),
}

/// A loaded Sieve script.
pub struct Rules {
    script: Sieve,
    runtime: Runtime,
    targets: Vec<String>,
    copy_only: HashSet<String>,
}

impl std::fmt::Debug for Rules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rules")
            .field("targets", &self.targets)
            .finish_non_exhaustive()
    }
}

impl Rules {
    /// Reads and compiles the script at `path`.
    ///
    /// # Errors
    /// Fails when the file can't be read or the script doesn't compile.
    pub fn load(path: &Path) -> Result<Self, Error> {
        Self::parse(&fs::read(path)?)
    }

    /// Compiles a script, so a mistake surfaces at startup and not mid-pass.
    ///
    /// # Errors
    /// Fails when the script doesn't compile.
    pub fn parse(source: &[u8]) -> Result<Self, Error> {
        let script = Compiler::new()
            .compile(source)
            .map_err(|err| Error::Parse(err.to_string()))?;

        let found = scan::file_intos(&String::from_utf8_lossy(source));
        let mut targets: Vec<String> = found.iter().map(|f| f.folder.clone()).collect();
        targets.sort_unstable();
        targets.dedup();

        let copy_only = targets
            .iter()
            .filter(|folder| {
                found
                    .iter()
                    .filter(|f| &f.folder == *folder)
                    .all(|f| f.copy)
            })
            .cloned()
            .collect();

        let rules = Self {
            script,
            runtime: Runtime::new(),
            targets,
            copy_only,
        };
        // The compiler accepts extensions the interpreter lacks and only
        // complains when the script runs, so run it once now.
        rules
            .folder(b"From: probe@example.org\r\n\r\n", "probe@example.org")
            .map_err(|err| Error::Parse(err.to_string()))?;
        Ok(rules)
    }

    /// The folders the script can file into, sorted and without repeats. A
    /// name built from a variable can't be known before the script runs and is
    /// left out.
    #[must_use]
    pub fn targets(&self) -> &[String] {
        &self.targets
    }

    /// Runs the script on a raw message and returns the folder its first
    /// `fileinto` names, or `None` when the message stays where it is.
    /// Redirects and every other action are ignored: only filing makes sense
    /// over IMAP.
    ///
    /// shortcut: the interpreter doesn't say which `fileinto` had `:copy`, so
    /// a folder counts as a copy only when every `fileinto` naming it does; a
    /// script that files into one folder both ways may pick the wrong one.
    ///
    /// # Errors
    /// Fails when the interpreter stops with an error.
    pub fn folder(&self, raw: &[u8], from: &str) -> Result<Option<String>, Error> {
        let raw = with_body_separator(raw);
        let mut instance = self.runtime.filter(&raw);
        instance.set_envelope(Envelope::From, from.to_owned());

        let mut input = Input::script("rules", self.script.clone());
        let mut first = None;

        while let Some(step) = instance.run(input) {
            input = match step.map_err(|err| Error::Run(format!("{err:?}")))? {
                Event::FileInto { folder, .. } => {
                    if first.is_none() && !self.copy_only.contains(&folder) {
                        first = Some(folder);
                    }
                    Input::True
                }
                Event::MailboxExists { .. }
                | Event::ListContains { .. }
                | Event::DuplicateId { .. }
                | Event::IncludeScript { .. }
                | Event::Function { .. } => Input::False,
                _ => Input::True,
            };
        }
        Ok(first)
    }
}

/// A message cut short in its headers has no blank line, which the
/// interpreter needs to find where the headers end.
fn with_body_separator(raw: &[u8]) -> Cow<'_, [u8]> {
    let has_separator =
        raw.windows(4).any(|w| w == b"\r\n\r\n") || raw.windows(2).any(|w| w == b"\n\n");
    if has_separator {
        return Cow::Borrowed(raw);
    }
    let mut padded = raw.to_vec();
    padded.extend_from_slice(b"\r\n\r\n");
    Cow::Owned(padded)
}

/// The lower-cased address in a raw message's From header, or "" when it has
/// none.
#[must_use]
pub fn sender_of(raw: &[u8]) -> String {
    MessageParser::default()
        .parse_headers(raw)
        .and_then(|message| {
            message
                .from()?
                .iter()
                .next()
                .and_then(Addr::address)
                .map(str::to_lowercase)
        })
        .unwrap_or_default()
}

/// A folder the script files into that the mailbox lacks, with the existing
/// name closest to it when one is within a couple of typos.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Absent {
    pub folder: String,
    pub nearest: Option<String>,
}

/// The targets that are not among `existing`, in order.
#[must_use]
pub fn missing(targets: &[String], existing: &[String]) -> Vec<Absent> {
    targets
        .iter()
        .filter(|target| !existing.contains(target))
        .map(|target| Absent {
            folder: target.clone(),
            nearest: nearest(target, existing),
        })
        .collect()
}

fn nearest(target: &str, existing: &[String]) -> Option<String> {
    let target = target.to_lowercase();
    existing
        .iter()
        .map(|name| {
            (
                edit_distance(target.as_bytes(), name.to_lowercase().as_bytes()),
                name,
            )
        })
        .filter(|(distance, _)| *distance <= MAX_TYPOS)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, name)| name.clone())
}

fn edit_distance(left: &[u8], right: &[u8]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, l) in left.iter().enumerate() {
        let mut current = vec![row + 1];
        for (col, r) in right.iter().enumerate() {
            let cost = usize::from(l != r);
            current.push(
                (previous[col + 1] + 1)
                    .min(current[col] + 1)
                    .min(previous[col] + cost),
            );
        }
        previous = current;
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = r#"require ["fileinto", "body"];
if header :contains "list-id" "golang-nuts" { fileinto "Lists/Go"; stop; }
if address :is "from" "billing@shop.example" { fileinto "Receipts"; }
if body :text :contains "invoice" { fileinto "Invoices"; }
"#;

    fn message(headers: &str, body: &str) -> Vec<u8> {
        format!("{headers}\r\n\r\n{body}").into_bytes()
    }

    fn folder(script: &str, raw: &[u8]) -> Option<String> {
        Rules::parse(script.as_bytes())
            .unwrap()
            .folder(raw, "someone@example.org")
            .unwrap()
    }

    #[test]
    fn files_by_header_address_and_body() {
        let cases = [
            (
                "header match",
                message(
                    "From: a@b.c\r\nList-Id: <golang-nuts.googlegroups.com>",
                    "hi",
                ),
                Some("Lists/Go"),
            ),
            (
                "address match",
                message("From: Shop <billing@shop.example>", "hi"),
                Some("Receipts"),
            ),
            (
                "body match",
                message("From: a@b.c", "Your invoice is attached"),
                Some("Invoices"),
            ),
            ("no match", message("From: a@b.c", "hello"), None),
            (
                "truncated headers",
                b"From: a@b.c\r\nList-Id: golang-nuts".to_vec(),
                Some("Lists/Go"),
            ),
        ];
        for (name, raw, want) in cases {
            assert_eq!(folder(SCRIPT, &raw).as_deref(), want, "{name}");
        }
    }

    #[test]
    fn redirects_and_other_actions_are_ignored() {
        let raw = message("From: a@b.c", "hi");
        for script in [
            r#"redirect "elsewhere@example.net";"#,
            "keep;",
            "discard;",
            r#"require "reject"; reject "no";"#,
        ] {
            assert_eq!(folder(script, &raw), None, "{script}");
        }
    }

    #[test]
    fn a_copy_does_not_decide_the_folder() {
        let raw = message("From: a@b.c", "hi");
        let script = r#"require ["fileinto", "copy"]; fileinto :copy "Archive"; fileinto "Real";"#;
        assert_eq!(folder(script, &raw).as_deref(), Some("Real"));

        let only_copy = r#"require ["fileinto", "copy"]; fileinto :copy "Archive";"#;
        assert_eq!(folder(only_copy, &raw), None);
    }

    #[test]
    fn the_envelope_sender_is_the_given_one() {
        let script = r#"require ["envelope", "fileinto"];
if envelope :is "from" "someone@example.org" { fileinto "Mine"; }"#;
        assert_eq!(
            folder(script, &message("From: a@b.c", "hi")).as_deref(),
            Some("Mine")
        );
    }

    #[test]
    fn a_bad_script_is_rejected() {
        for script in [r#"fileinto "X";"#, "if {", r#"require "nonsense";"#] {
            assert!(Rules::parse(script.as_bytes()).is_err(), "{script}");
        }
    }

    #[test]
    fn load_reads_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.sieve");
        std::fs::write(&path, SCRIPT).unwrap();

        assert!(Rules::load(&path).is_ok());
        assert!(Rules::load(&dir.path().join("missing.sieve")).is_err());
    }

    #[test]
    fn targets_are_sorted_distinct_and_literal() {
        let cases: [(&str, &str, &[&str]); 5] = [
            ("none", "keep;", &[]),
            (
                "nested, listed and repeated",
                r#"require ["fileinto", "copy", "variables"];
if header :contains "a" "b" { fileinto "B"; if true { fileinto :copy "C"; } }
elsif true { fileinto "A"; } else { fileinto "B"; }
set "dir" "Dyn";
fileinto "${dir}/x";"#,
                &["A", "B", "C"],
            ),
            (
                "comments and strings are not commands",
                "require \"fileinto\";\n# fileinto \"Nope\";\n/* fileinto \"Nope2\"; */\nif header :contains \"x\" \"fileinto \\\"Nope3\\\"\" { fileinto \"Yes\"; }",
                &["Yes"],
            ),
            (
                "tagged arguments are skipped",
                r#"require ["fileinto", "imap4flags", "mailbox"];
fileinto :flags ["\\Seen", "x"] :create "Flagged";"#,
                &["Flagged"],
            ),
            (
                "multi-line text is not scanned for commands",
                "require \"fileinto\";\nif header :contains \"x\" text:\nfileinto \"Nope\";\n.\n { fileinto \"Yes\"; }",
                &["Yes"],
            ),
        ];
        for (name, script, want) in cases {
            let rules = Rules::parse(script.as_bytes());
            let rules = rules.unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!(rules.targets(), want, "{name}");
        }
    }

    #[test]
    fn senders_come_from_the_from_header() {
        let cases = [
            (
                "named",
                "From: Gopher <G@Lists.Example>\r\n\r\nhi",
                "g@lists.example",
            ),
            ("bare", "From: a@b.example\r\n\r\nhi", "a@b.example"),
            ("missing", "Subject: x\r\n\r\nhi", ""),
            ("garbage", "not a message", ""),
        ];
        for (name, raw, want) in cases {
            assert_eq!(sender_of(raw.as_bytes()), want, "{name}");
        }
    }

    #[test]
    fn missing_folders_suggest_the_nearest_name() {
        let targets = ["INBOX", "Elsewhere", "jobsearh"].map(String::from);
        let existing = ["INBOX", "jobsearch", "code"].map(String::from);

        assert_eq!(
            missing(&targets, &existing),
            [
                Absent {
                    folder: "Elsewhere".into(),
                    nearest: None
                },
                Absent {
                    folder: "jobsearh".into(),
                    nearest: Some("jobsearch".into())
                },
            ]
        );
        assert_eq!(
            missing(&["code".to_owned()], &["code".to_owned()]),
            Vec::<Absent>::new()
        );
    }
}
