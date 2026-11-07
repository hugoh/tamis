//! Files mail into folders by running a Sieve script (RFC 5228) against it,
//! the way a server-side filter would.

mod scan;

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use mail_parser::MessageParser;
use serde::Serialize;
use sieve::compiler::grammar::Capability;
use sieve::{Compiler, Envelope, Event, Input, Recipient, Runtime, Sieve};
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

/// The Sieve extensions a script can `require` and tamis honours: everything
/// the interpreter knows, minus what only matters to a mail server.
#[must_use]
pub fn supported_extensions() -> Vec<String> {
    let mut names: Vec<String> = Capability::all()
        .iter()
        .map(ToString::to_string)
        .filter(|name| !scan::UNSUPPORTED_EXTENSIONS.contains(&name.as_str()))
        .collect();
    names.sort_unstable();
    names
}

/// Where each extension is specified: an RFC, or the draft for `regex`.
#[must_use]
pub fn extension_reference(name: &str) -> Option<String> {
    let rfc = match name {
        "body" => 5173,
        "copy" => 3894,
        "date" | "index" => 5260,
        "comparator-i;ascii-casemap"
        | "comparator-i;ascii-numeric"
        | "comparator-i;octet"
        | "encoded-character"
        | "envelope"
        | "fileinto" => 5228,
        "envelope-deliverby" | "envelope-dsn" => 6009,
        "environment" => 5183,
        "extracttext" | "foreverypart" | "mime" => 5703,
        "ihave" => 5463,
        "imap4flags" => 5232,
        "mailbox" => 5490,
        "mailboxid" => 9042,
        "regex" => {
            return Some(
                "https://datatracker.ietf.org/doc/draft-murchison-sieve-regex/".to_string(),
            );
        }
        "relational" => 5231,
        "special-use" => 8579,
        "subaddress" => 5233,
        "variables" => 5229,
        _ => return None,
    };
    Some(format!("https://www.rfc-editor.org/rfc/rfc{rfc}"))
}

/// What a script decides for one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    /// The first `fileinto` without `:copy`, or `None` when the message stays.
    pub folder: Option<String>,
    /// The IMAP flags and keywords to set: those of the `fileinto` that
    /// decided the folder, or of the keep when the message stays.
    pub flags: Vec<String>,
    /// Actions the script took that tamis does not carry out, such as a
    /// `redirect`, a `discard` or a second `fileinto`.
    pub ignored: Vec<String>,
}

/// A loaded Sieve script.
pub struct Rules {
    script: Sieve,
    runtime: Runtime,
    targets: Vec<String>,
    copy_only: HashSet<String>,
    unsupported: Vec<String>,
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

        let text = String::from_utf8_lossy(source);
        let unsupported = scan::unsupported(&text);
        let found = scan::file_intos(&text);
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
            unsupported,
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

    /// The commands the script uses that tamis ignores or denies (`redirect`,
    /// `reject`, `vacation`, `discard`, `keep`, flags, `fileinto :copy`, ...),
    /// sorted. Found by scanning the text, so it covers branches no message
    /// has taken yet.
    #[must_use]
    pub fn unsupported(&self) -> &[String] {
        &self.unsupported
    }

    /// Runs the script on a raw message and returns the folder its first
    /// `fileinto` names, or `None` when the message stays where it is.
    /// Redirects and every other action are ignored: only filing makes sense
    /// over IMAP. [`Rules::decide`] also says what was ignored.
    ///
    /// # Errors
    /// Fails when the interpreter stops with an error.
    pub fn folder(&self, raw: &[u8], from: &str) -> Result<Option<String>, Error> {
        Ok(self.decide(raw, from)?.folder)
    }

    /// Runs the script on a raw message and returns what tamis would do and
    /// what the script asked for that it will not.
    ///
    /// shortcut: the interpreter doesn't say which `fileinto` had `:copy`, so
    /// a folder counts as a copy only when every `fileinto` naming it does; a
    /// script that files into one folder both ways may pick the wrong one.
    ///
    /// # Errors
    /// Fails when the interpreter stops with an error.
    pub fn decide(&self, raw: &[u8], from: &str) -> Result<Decision, Error> {
        let raw = with_body_separator(raw);
        let mut instance = self.runtime.filter(&raw);
        instance.set_envelope(Envelope::From, from.to_owned());

        let mut input = Input::script("rules", self.script.clone());
        let mut decision = Decision {
            folder: None,
            flags: Vec::new(),
            ignored: Vec::new(),
        };
        let mut keeps = Vec::new();
        let mut keep_flags = Vec::new();

        while let Some(step) = instance.run(input) {
            input = match step.map_err(|err| Error::Run(format!("{err:?}")))? {
                Event::FileInto { folder, flags, .. } => {
                    if self.copy_only.contains(&folder) {
                        decision.ignored.push(format!("fileinto {folder} :copy"));
                    } else if decision.folder.is_none() {
                        decision.flags = flags;
                        decision.folder = Some(folder);
                    } else {
                        decision
                            .ignored
                            .push(format!("fileinto {folder} (after the first)"));
                    }
                    Input::True
                }
                // The interpreter also reports the implicit keep, so a keep only
                // counts as ignored when it leaves a copy next to a `fileinto`.
                Event::Keep { flags, .. } => {
                    keeps.push(with_flags("keep", &flags));
                    keep_flags = flags;
                    Input::True
                }
                Event::Discard => {
                    decision.ignored.push("discard".to_owned());
                    Input::True
                }
                Event::Reject { extended, .. } => {
                    decision
                        .ignored
                        .push(if extended { "ereject" } else { "reject" }.to_owned());
                    Input::True
                }
                Event::SendMessage { recipient, .. } => {
                    decision
                        .ignored
                        .push(format!("send message to {}", describe(&recipient)));
                    Input::True
                }
                Event::Notify { method, .. } => {
                    decision.ignored.push(format!("notify {method}"));
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
        if decision.folder.is_some() {
            keeps.dedup();
            decision.ignored.extend(keeps);
        } else {
            decision.flags = keep_flags;
        }
        Ok(decision)
    }
}

fn with_flags(action: &str, flags: &[String]) -> String {
    if flags.is_empty() {
        action.to_owned()
    } else {
        format!("{action} with flags {}", flags.join(" "))
    }
}

fn describe(recipient: &Recipient) -> String {
    match recipient {
        Recipient::Address(address) | Recipient::List(address) => address.clone(),
        Recipient::Group(addresses) => addresses.join(", "),
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

/// The trimmed value of the first header called `name` (case-insensitive) in a
/// raw message, or `None` when it has none. Folded continuation lines are not
/// joined.
#[must_use]
pub fn header_value(raw: &[u8], name: &str) -> Option<String> {
    for line in raw.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return None;
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        if line[..colon].eq_ignore_ascii_case(name.as_bytes()) {
            return Some(
                String::from_utf8_lossy(&line[colon + 1..])
                    .trim()
                    .to_owned(),
            );
        }
    }
    None
}

/// The lower-cased address in a raw message's From header, or "" when it has
/// none.
#[must_use]
pub fn sender_of(raw: &[u8]) -> String {
    MessageParser::default()
        .parse_headers(raw)
        .and_then(|message| message.from()?.first()?.address().map(str::to_lowercase))
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
    fn a_decision_lists_what_the_script_asked_for_beyond_filing() {
        let script = "require [\"fileinto\", \"imap4flags\", \"copy\"];\n\
            fileinto :copy \"Copy\";\n\
            fileinto :flags \"\\\\Flagged\" \"A\";\n\
            fileinto \"B\";\n\
            redirect \"x@y.z\";\n\
            discard;\n";
        let rules = Rules::parse(script.as_bytes()).unwrap();
        let decision = rules
            .decide(&message("From: a@b.c", "hi"), "a@b.c")
            .unwrap();
        assert_eq!(decision.folder.as_deref(), Some("A"));
        assert_eq!(decision.flags, ["\\Flagged"]);
        assert_eq!(
            decision.ignored,
            [
                "fileinto Copy :copy",
                "fileinto B (after the first)",
                "send message to x@y.z",
                "discard",
            ]
        );
        assert_eq!(
            rules.unsupported(),
            ["discard", "fileinto :copy", "redirect"]
        );
    }

    #[test]
    fn supported_extensions_leave_out_what_tamis_cannot_honour() {
        let supported = supported_extensions();
        for name in [
            "fileinto",
            "body",
            "regex",
            "variables",
            "relational",
            "imap4flags",
        ] {
            assert!(supported.iter().any(|e| e == name), "{name}");
        }
        for name in ["vacation", "reject", "editheader", "include"] {
            assert!(!supported.iter().any(|e| e == name), "{name}");
        }
        assert!(supported.is_sorted());
    }

    #[test]
    fn a_required_extension_tamis_cannot_honour_is_unsupported() {
        let rules = Rules::parse(
            b"require [\"fileinto\", \"reject\"];\nrequire \"vacation\";\nfileinto \"A\";",
        )
        .unwrap();
        assert_eq!(
            rules.unsupported(),
            ["extension reject", "extension vacation"]
        );
    }

    #[test]
    fn flags_follow_the_fileinto_or_the_keep() {
        let raw = message("From: a@b.c", "hi");
        let cases = [
            (
                "filed",
                r#"require ["fileinto", "imap4flags"]; setflag "\\Seen"; fileinto "A";"#,
                Some("A"),
                vec!["\\Seen"],
            ),
            (
                "staying",
                r#"require ["imap4flags"]; addflag ["\\Flagged", "x"];"#,
                None,
                vec!["\\Flagged", "x"],
            ),
            (
                "per fileinto",
                r#"require ["fileinto", "imap4flags"]; fileinto :flags "\\Seen" "A";"#,
                Some("A"),
                vec!["\\Seen"],
            ),
            (
                "none",
                r#"require ["fileinto"]; fileinto "A";"#,
                Some("A"),
                vec![],
            ),
        ];
        for (name, script, folder, flags) in cases {
            let decision = Rules::parse(script.as_bytes())
                .unwrap()
                .decide(&raw, "a@b.c")
                .unwrap();
            assert_eq!(decision.folder.as_deref(), folder, "{name}");
            assert_eq!(decision.flags, flags, "{name}");
            assert!(
                decision.ignored.is_empty(),
                "{name}: {:?}",
                decision.ignored
            );
        }
    }

    #[test]
    fn the_implicit_keep_is_not_reported() {
        let rules = Rules::parse(b"require [\"fileinto\"];\nif false { fileinto \"A\"; }").unwrap();
        let decision = rules
            .decide(&message("From: a@b.c", "hi"), "a@b.c")
            .unwrap();
        assert_eq!(
            decision,
            Decision {
                folder: None,
                flags: Vec::new(),
                ignored: Vec::new()
            }
        );
        assert!(rules.unsupported().is_empty());
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
