//! Checks which IMAP features a mail server actually supports, so tamis can
//! pick a sync strategy before it touches real mail.

use std::io::Write;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde::Serialize;

use crate::mail::client::{Client, DialConfig};
use crate::mail::{Error, Mailbox};
use crate::report::write_table;

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

/// Probes the server. It returns an error only when it cannot connect or log
/// in; everything else is recorded as a check in the report.
///
/// # Errors
/// When the server can't be reached or rejects the login.
pub async fn run(config: &Config) -> Result<Report, Error> {
    let mut report = Report {
        server: format!("{}:{}", config.host, config.port),
        ..Report::default()
    };

    let mut client = Client::dial(&config.dial()).await?;
    report.add(
        "login",
        Status::Ok,
        format!("authenticated as {}", config.username),
    );

    report.capabilities = client.capabilities();
    report.capabilities.sort_unstable();
    check_capabilities(&mut report);
    check_folders(&mut report, &mut client).await;
    check_inbox(&mut report, &mut client).await;

    if config.write {
        write_checks(&mut report, config, &mut client).await;
    } else {
        report.add(
            "write checks",
            Status::Skip,
            "rerun with --write to test APPEND, IDLE, MOVE",
        );
    }
    client.close().await;
    Ok(report)
}

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

async fn check_folders(report: &mut Report, client: &mut Client) {
    let listed = match client.list_with_attributes().await {
        Ok(listed) => listed,
        Err(err) => {
            report.add("LIST", Status::Fail, err.to_string());
            return;
        }
    };
    report.folders = listed
        .into_iter()
        .map(|(name, attrs)| Folder { name, attrs })
        .collect();
    report.add(
        "LIST",
        Status::Ok,
        format!("{} folders", report.folders.len()),
    );

    check_special_folder(report, "\\Junk", &["Junk"]);
    check_special_folder(report, "\\Sent", &["Sent Messages", "Sent"]);
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

async fn check_inbox(report: &mut Report, client: &mut Client) {
    let examined = match client.examine("INBOX").await {
        Ok(examined) => examined,
        Err(err) => {
            report.add("EXAMINE INBOX", Status::Fail, err.to_string());
            return;
        }
    };
    report.add(
        "EXAMINE INBOX",
        Status::Ok,
        format!(
            "{} messages, UIDVALIDITY {}, UIDNEXT {}",
            examined.messages, examined.uid_validity, examined.uid_next
        ),
    );
    if examined.keywords_allowed {
        report.add("custom keywords", Status::Ok, "PERMANENTFLAGS includes \\*");
    } else {
        report.add(
            "custom keywords",
            Status::Warn,
            "server may not keep keywords such as $TamisFiled",
        );
    }
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

async fn write_checks(report: &mut Report, config: &Config, client: &mut Client) {
    let scratch = Scratch::new(Timestamp::now());
    if !create_scratch(report, client, &scratch).await {
        return;
    }

    run_on_scratch(report, config, client, &scratch).await;
    delete_scratch(report, client, &scratch).await;
}

async fn create_scratch(report: &mut Report, client: &mut Client, scratch: &Scratch) -> bool {
    for name in [&scratch.src, &scratch.dst] {
        if let Err(err) = client.create_folder(name).await {
            report.add("CREATE scratch", Status::Fail, format!("{name}: {err}"));
            return false;
        }
    }
    report.add(
        "CREATE scratch",
        Status::Ok,
        format!("{}, {}", scratch.src, scratch.dst),
    );
    true
}

async fn delete_scratch(report: &mut Report, client: &mut Client, scratch: &Scratch) {
    // A folder can't be deleted while selected on some servers.
    let _ = client.examine("INBOX").await;
    let mut failed = Vec::new();
    for name in [&scratch.src, &scratch.dst] {
        if let Err(err) = client.delete_folder(name).await {
            failed.push(format!("{name}: {err}"));
        }
    }
    if failed.is_empty() {
        report.add("DELETE scratch", Status::Ok, "scratch folders removed");
    } else {
        report.add(
            "DELETE scratch",
            Status::Warn,
            format!("delete by hand: {failed:?}"),
        );
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

async fn run_on_scratch(
    report: &mut Report,
    config: &Config,
    client: &mut Client,
    scratch: &Scratch,
) {
    let message = test_message(Timestamp::now());
    let uid = match client.append(&scratch.src, &message, &[]).await {
        Err(err) => {
            report.add("APPEND", Status::Fail, err.to_string());
            return;
        }
        Ok(None) => {
            report.add(
                "APPEND",
                Status::Fail,
                "no APPENDUID: remaining write checks skipped",
            );
            return;
        }
        Ok(Some(uid)) => uid,
    };
    report.add(
        "APPEND",
        Status::Ok,
        format!("APPENDUID returned UID {uid}"),
    );

    check_peek(report, client, &scratch.src, uid).await;
    check_keyword(report, client, &scratch.src, uid).await;
    check_idle(report, config, client, &scratch.src).await;
    check_move(report, client, &scratch.src, &scratch.dst, uid).await;
}

/// The flags the message has now.
async fn flags_of(client: &mut Client, folder: &str, uid: u32) -> Result<Vec<String>, String> {
    let found = client
        .fetch_envelopes(folder, &[uid])
        .await
        .map_err(|err| format!("fetch flags: {err}"))?;
    found
        .into_iter()
        .next()
        .map(|m| m.flags)
        .ok_or_else(|| format!("fetch flags: UID {uid}: message not found"))
}

/// Runs an action on the test message, then checks whether a flag ended up
/// set.
struct FlagCheck<'a> {
    name: &'a str,
    flag: &'a str,
    want_set: bool,
    ok: String,
    bad: String,
}

impl FlagCheck<'_> {
    async fn finish(&self, report: &mut Report, client: &mut Client, folder: &str, uid: u32) {
        match flags_of(client, folder, uid).await {
            Err(err) => report.add(self.name, Status::Fail, err),
            Ok(flags)
                if flags.iter().any(|f| f.eq_ignore_ascii_case(self.flag)) == self.want_set =>
            {
                report.add(self.name, Status::Ok, self.ok.clone());
            }
            Ok(_) => report.add(self.name, Status::Fail, self.bad.clone()),
        }
    }
}

/// Confirms `BODY.PEEK[]` leaves the message unread, so scanning mail never
/// marks it as seen.
async fn check_peek(report: &mut Report, client: &mut Client, folder: &str, uid: u32) {
    // Fetching the body is exactly what the engine does.
    if let Err(err) = client.fetch(folder, &[uid]).await {
        report.add("BODY.PEEK", Status::Fail, err.to_string());
        return;
    }
    let check = FlagCheck {
        name: "BODY.PEEK",
        flag: "\\Seen",
        want_set: false,
        ok: "message still unread after BODY.PEEK[]".to_owned(),
        bad: "peek set \\Seen: scanning would mark mail as read".to_owned(),
    };
    check.finish(report, client, folder, uid).await;
}

/// Confirms the server stores custom keywords, which tamis uses to recognise
/// messages it has already filed.
async fn check_keyword(report: &mut Report, client: &mut Client, folder: &str, uid: u32) {
    if let Err(err) = client.add_keyword(folder, &[uid], PROBE_KEYWORD).await {
        report.add("STORE keyword", Status::Fail, err.to_string());
        return;
    }
    let check = FlagCheck {
        name: "STORE keyword",
        flag: PROBE_KEYWORD,
        want_set: true,
        ok: format!("{PROBE_KEYWORD} kept"),
        bad: format!("{PROBE_KEYWORD} silently dropped"),
    };
    check.finish(report, client, folder, uid).await;
}

/// Opens a second connection that IDLEs on the scratch folder, then appends
/// from the first and times how long the push takes. It also shows the server
/// accepts two concurrent connections.
async fn check_idle(report: &mut Report, config: &Config, client: &mut Client, folder: &str) {
    const NAME: &str = "IDLE push";
    if !advertised(report, "IDLE") {
        report.add(NAME, Status::Skip, "IDLE not advertised");
        return;
    }

    let mut watcher = match Client::dial(&config.dial()).await {
        Ok(watcher) => watcher,
        Err(err) => {
            report.add("second connection", Status::Fail, err.to_string());
            return;
        }
    };
    report.add(
        "second connection",
        Status::Ok,
        "two concurrent logins accepted",
    );

    let wait = config.idle_wait;
    let target = folder.to_owned();
    let idling = tokio::spawn(async move {
        let outcome = watcher.idle(&target, wait).await;
        (watcher, outcome)
    });

    // Give the watcher a moment to get into IDLE before the mail arrives.
    tokio::time::sleep(Duration::from_millis(750)).await;
    let started = Instant::now();
    let (status, detail) = match client
        .append(folder, &test_message(Timestamp::now()), &[])
        .await
    {
        Err(err) => (
            Status::Fail,
            format!("append failed, push not tested: {err}"),
        ),
        Ok(_) => match idling.await {
            Ok((_, Ok(true))) => (
                Status::Ok,
                format!(
                    "new mail pushed after {:?}",
                    started.elapsed().round_to_millis()
                ),
            ),
            Ok((_, Ok(false))) => (
                Status::Warn,
                format!("no push within {wait:?}: poll this folder instead"),
            ),
            Ok((_, Err(err))) => (Status::Fail, err.to_string()),
            Err(err) => (Status::Fail, format!("idle task: {err}")),
        },
    };
    report.add(NAME, status, detail);
}

trait RoundToMillis {
    fn round_to_millis(self) -> Duration;
}

impl RoundToMillis for Duration {
    fn round_to_millis(self) -> Duration {
        Duration::from_millis(u64::try_from(self.as_millis()).unwrap_or(u64::MAX))
    }
}

async fn check_move(report: &mut Report, client: &mut Client, src: &str, dst: &str, uid: u32) {
    const NAME: &str = "MOVE message";
    let method = client.move_method();
    if let Err(err) = client.move_messages(src, &[uid], dst).await {
        report.add(NAME, Status::Fail, format!("{method}: {err}"));
        return;
    }
    match client.messages(dst).await {
        Err(err) => report.add(NAME, Status::Fail, format!("{method}: {err}")),
        Ok(1) => report.add(NAME, Status::Ok, method),
        Ok(_) => report.add(
            NAME,
            Status::Fail,
            format!("{method}: message not found in {dst}"),
        ),
    }
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
