//! Checks which IMAP features a mail server actually supports, so tamis can
//! pick a sync strategy before it touches real mail.

use std::io::Write;
use std::time::Duration;

use jiff::Timestamp;
use serde::Serialize;

use crate::mail::client::DialConfig;
use crate::report::write_table;

mod live;

pub use live::run;

/// The outcome of a single check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Skip,
}

impl Status {
    /// Problems stand out in a terminal.
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "skip",
        }
    }
}

/// What to probe.
#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Also create, fill and delete scratch folders.
    pub write: bool,
    /// How long to wait for an IDLE push before giving up.
    pub idle_wait: Duration,
    /// Dials without TLS. Only for local test servers.
    pub plaintext: bool,
}

impl Config {
    fn dial(&self) -> DialConfig {
        DialConfig {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            password: self.password.clone(),
            plaintext: self.plaintext,
        }
    }
}

/// The result of probing one feature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// A folder as reported by `LIST`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Folder {
    pub name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attrs: Vec<String>,
}

/// Everything the probe learned.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub server: String,
    pub capabilities: Vec<String>,
    #[serde(rename = "mailboxes")]
    pub folders: Vec<Folder>,
    pub checks: Vec<Check>,
}

impl Report {
    /// Whether any check failed.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.checks.iter().any(|check| check.status == Status::Fail)
    }

    fn add(&mut self, name: &str, status: Status, detail: impl Into<String>) {
        self.checks.push(Check {
            name: name.to_owned(),
            status,
            detail: detail.into(),
        });
    }

    /// Renders the report for a terminal.
    ///
    /// # Errors
    /// When `out` can't be written.
    pub fn write_text(&self, out: &mut impl Write) -> std::io::Result<()> {
        writeln!(out, "Server: {}", self.server)?;
        writeln!(out, "Capabilities: {}\n", self.capabilities.join(" "))?;

        let rows: Vec<Vec<String>> = self
            .checks
            .iter()
            .map(|c| {
                vec![
                    c.status.label().to_owned(),
                    c.name.clone(),
                    c.detail.clone(),
                ]
            })
            .collect();
        write_table(out, &rows)?;

        if !self.folders.is_empty() {
            writeln!(out, "\nFolders:")?;
            let rows: Vec<Vec<String>> = self
                .folders
                .iter()
                .map(|f| vec![format!("  {}", f.name), f.attrs.join(" ")])
                .collect();
            write_table(out, &rows)?;
        }
        Ok(())
    }
}

/// An extension tamis cares about, and why.
struct Wanted {
    name: &'static str,
    why: &'static str,
    required: bool,
}

const WANTED: [Wanted; 4] = [
    Wanted {
        name: "IDLE",
        why: "push notification of new mail",
        required: true,
    },
    Wanted {
        name: "UIDPLUS",
        why: "safe UID EXPUNGE and APPENDUID",
        required: true,
    },
    Wanted {
        name: "MOVE",
        why: "atomic moves (otherwise COPY + STORE + EXPUNGE)",
        required: false,
    },
    Wanted {
        name: "SPECIAL-USE",
        why: "automatic Junk/Sent folder discovery",
        required: false,
    },
];

const PROBE_KEYWORD: &str = "$TamisProbe";

fn advertised(report: &Report, name: &str) -> bool {
    report
        .capabilities
        .iter()
        .any(|cap| cap.eq_ignore_ascii_case(name))
}

fn check_capabilities(report: &mut Report) {
    for want in &WANTED {
        let (status, detail) = if advertised(report, want.name) {
            (Status::Ok, format!("advertised: {}", want.why))
        } else if want.required {
            (Status::Fail, format!("missing: {}", want.why))
        } else {
            (Status::Warn, format!("missing: {}", want.why))
        };
        report.add(want.name, status, detail);
    }
}

/// Looks for a folder by SPECIAL-USE attribute, falling back to well-known
/// names.
fn check_special_folder(report: &mut Report, attribute: &str, names: &[&str]) {
    let name = format!("folder {attribute}");
    let by_attribute = report
        .folders
        .iter()
        .find(|f| f.attrs.iter().any(|a| a.eq_ignore_ascii_case(attribute)));
    if let Some(folder) = by_attribute {
        let detail = format!("{:?}", folder.name);
        report.add(&name, Status::Ok, detail);
        return;
    }
    let by_name = report
        .folders
        .iter()
        .find(|f| names.contains(&f.name.as_str()));
    if let Some(folder) = by_name {
        let detail = format!("no attribute; guessed {:?} by name", folder.name);
        report.add(&name, Status::Warn, detail);
        return;
    }
    report.add(&name, Status::Warn, "not found");
}

/// The two throwaway folders the write checks use. They are top-level and
/// timestamped so they never collide with real mail.
struct Scratch {
    src: String,
    dst: String,
}

impl Scratch {
    fn new(now: Timestamp) -> Self {
        let base = format!("tamis-probe-{}", now.as_second());
        Self {
            dst: format!("{base}-moved"),
            src: base,
        }
    }
}

fn test_message(now: Timestamp) -> Vec<u8> {
    format!(
        "From: tamis probe <probe@tamis.invalid>\r\nTo: probe@tamis.invalid\r\nSubject: tamis probe\r\n\
         Date: {}\r\nMessage-ID: <{}.probe@tamis.invalid>\r\n\r\nTest message written by tamis probe. Safe to delete.\r\n",
        now.strftime("%a, %d %b %Y %H:%M:%S +0000"),
        now.as_nanosecond(),
    )
    .into_bytes()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn report(checks: &[(&str, Status, &str)]) -> Report {
        Report {
            server: "imap.example.org:993".into(),
            capabilities: vec!["IDLE".into(), "UIDPLUS".into()],
            folders: vec![
                Folder {
                    name: "INBOX".into(),
                    attrs: vec![],
                },
                Folder {
                    name: "Junk".into(),
                    attrs: vec!["\\Junk".into()],
                },
            ],
            checks: checks
                .iter()
                .map(|(name, status, detail)| Check {
                    name: (*name).into(),
                    status: *status,
                    detail: (*detail).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn missing_extensions_are_failures_only_when_required() {
        let mut full = Report {
            capabilities: vec![
                "IDLE".into(),
                "uidplus".into(),
                "MOVE".into(),
                "SPECIAL-USE".into(),
            ],
            ..Report::default()
        };
        check_capabilities(&mut full);
        assert!(
            full.checks.iter().all(|c| c.status == Status::Ok),
            "{:?}",
            full.checks
        );

        let mut bare = Report {
            capabilities: vec!["IMAP4rev1".into()],
            ..Report::default()
        };
        check_capabilities(&mut bare);
        let statuses: Vec<(&str, Status)> = bare
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.status))
            .collect();
        assert_eq!(
            statuses,
            [
                ("IDLE", Status::Fail),
                ("UIDPLUS", Status::Fail),
                ("MOVE", Status::Warn),
                ("SPECIAL-USE", Status::Warn)
            ]
        );
        assert!(bare.failed());
    }

    #[test]
    fn special_folders_are_found_by_attribute_then_by_name() {
        let mut found = Report {
            folders: vec![
                Folder {
                    name: "Spam!".into(),
                    attrs: vec!["\\Junk".into()],
                },
                Folder {
                    name: "Sent".into(),
                    attrs: vec![],
                },
            ],
            ..Report::default()
        };
        check_special_folder(&mut found, "\\Junk", &["Junk"]);
        check_special_folder(&mut found, "\\Sent", &["Sent Messages", "Sent"]);
        check_special_folder(&mut found, "\\Trash", &["Trash"]);

        let seen: Vec<(Status, &str)> = found
            .checks
            .iter()
            .map(|c| (c.status, c.detail.as_str()))
            .collect();
        assert_eq!(
            seen,
            [
                (Status::Ok, "\"Spam!\""),
                (Status::Warn, "no attribute; guessed \"Sent\" by name"),
                (Status::Warn, "not found"),
            ]
        );
    }

    #[test]
    fn the_report_reads_well_in_a_terminal() {
        let mut out = Vec::new();

        report(&[
            ("IDLE", Status::Ok, "advertised"),
            ("MOVE", Status::Warn, "missing"),
        ])
        .write_text(&mut out)
        .unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Server: imap.example.org:993\nCapabilities: IDLE UIDPLUS\n\nok    IDLE  advertised\nWARN  MOVE  missing\n\nFolders:\n  INBOX  \n  Junk   \\Junk\n"
        );
    }

    #[test]
    fn the_report_serializes_as_the_go_one_did() {
        let json = serde_json::to_string(&report(&[("IDLE", Status::Skip, "")])).unwrap();

        assert_eq!(
            json,
            r#"{"server":"imap.example.org:993","capabilities":["IDLE","UIDPLUS"],"mailboxes":[{"name":"INBOX"},{"name":"Junk","attrs":["\\Junk"]}],"checks":[{"name":"IDLE","status":"skip"}]}"#
        );
    }

    #[test]
    fn scratch_folders_are_timestamped_and_top_level() {
        let scratch = Scratch::new(Timestamp::from_second(1_700_000_000).unwrap());

        assert_eq!(
            (scratch.src.as_str(), scratch.dst.as_str()),
            ("tamis-probe-1700000000", "tamis-probe-1700000000-moved")
        );
    }

    #[test]
    fn the_test_message_is_a_parseable_probe() {
        let raw = test_message(Timestamp::from_second(1_700_000_000).unwrap());
        let text = String::from_utf8(raw).unwrap();

        assert!(text.starts_with("From: tamis probe <probe@tamis.invalid>\r\n"));
        assert!(
            text.contains("Date: Tue, 14 Nov 2023 22:13:20 +0000\r\n")
                && text.ends_with("Safe to delete.\r\n")
        );
    }
}
