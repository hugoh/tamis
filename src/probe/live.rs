//! The checks that talk to a server. Dovecot exercises them under
//! `mise run test-imap`.

use std::time::{Duration, Instant};

use jiff::Timestamp;

use super::{
    Config, Folder, PROBE_KEYWORD, Report, Scratch, Status, advertised, check_capabilities,
    check_special_folder, test_message,
};
use crate::mail::client::Client;
use crate::mail::{Error, Mailbox};

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

pub(super) async fn check_folders(report: &mut Report, client: &mut Client) {
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

pub(super) async fn check_inbox(report: &mut Report, client: &mut Client) {
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

pub(super) async fn write_checks(report: &mut Report, config: &Config, client: &mut Client) {
    let scratch = Scratch::new(Timestamp::now());
    if !create_scratch(report, client, &scratch).await {
        return;
    }

    run_on_scratch(report, config, client, &scratch).await;
    delete_scratch(report, client, &scratch).await;
}

pub(super) async fn create_scratch(
    report: &mut Report,
    client: &mut Client,
    scratch: &Scratch,
) -> bool {
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

pub(super) async fn delete_scratch(report: &mut Report, client: &mut Client, scratch: &Scratch) {
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

pub(super) async fn run_on_scratch(
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
pub(super) async fn flags_of(
    client: &mut Client,
    folder: &str,
    uid: u32,
) -> Result<Vec<String>, String> {
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
pub(super) async fn check_peek(report: &mut Report, client: &mut Client, folder: &str, uid: u32) {
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
pub(super) async fn check_keyword(
    report: &mut Report,
    client: &mut Client,
    folder: &str,
    uid: u32,
) {
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
pub(super) async fn check_idle(
    report: &mut Report,
    config: &Config,
    client: &mut Client,
    folder: &str,
) {
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

pub(super) async fn check_move(
    report: &mut Report,
    client: &mut Client,
    src: &str,
    dst: &str,
    uid: u32,
) {
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
