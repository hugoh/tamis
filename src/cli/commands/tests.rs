use std::path::Path;

use jiff::Timestamp;

use super::super::testing::{Harness, http_server};
use crate::store::{Decision, Store};

/// A configuration with its state in `dir`, and the state database.
fn setup(dir: &Path, extra: &str) -> (String, Store) {
    let path = dir.join("config.toml");
    let state = dir.join("state.db");
    std::fs::write(
        &path,
        format!("state = \"{}\"\n[account]\nhost = \"imap.example.org\"\nuser = \"me@example.org\"\n{extra}", state.display()),
    )
    .unwrap();
    (
        path.to_string_lossy().into_owned(),
        Store::open(&state).unwrap(),
    )
}

async fn run(args: &[&str]) -> (i32, String, String) {
    let (code, out, err) = Harness::new(&[], "").run(args).await;
    (code, out.text(), err.text())
}

fn decision(
    verdict: &str,
    action: &str,
    applied: bool,
    message_id: &str,
    reason: &str,
) -> Decision {
    Decision {
        at: Timestamp::now(),
        mailbox: "INBOX".into(),
        message_id: message_id.into(),
        from: "zed@nowhere.example".into(),
        subject: "Hello".into(),
        score: 0.987,
        verdict: verdict.parse().unwrap(),
        reason: reason.parse().unwrap(),
        action: action.parse().unwrap(),
        applied,
        ..Decision::default()
    }
}

#[tokio::test]
async fn log_shows_decisions_newest_first_with_what_happened_to_them() {
    let dir = tempfile::tempdir().unwrap();
    let (config, store) = setup(dir.path(), "");
    let (code, out, _) = run(&["-c", &config, "log"]).await;
    assert_eq!((code, out.as_str()), (0, "No decisions yet.\n"));

    store
        .record_decision(&decision("spam", "move:Junk", true, "<a@x>", "bayes"))
        .unwrap();
    store
        .record_decision(&decision("good", "move:Receipts", false, "<b@x>", "rule"))
        .unwrap();
    store
        .record_decision(&decision("good", "none", false, "<c@x>", "bayes"))
        .unwrap();
    let first = store.decisions(10).unwrap().last().unwrap().id;
    store.mark_undone(first).unwrap();
    store
        .record_decision(&decision("good", "train:good", true, "<b@x>", "correction"))
        .unwrap();

    let (code, out, _) = run(&["-c", &config, "--color", "never", "log"]).await;

    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].starts_with("ID") && lines[0].contains("VERDICT") && lines[0].ends_with("SUBJECT"),
        "{}",
        lines[0]
    );
    assert_eq!(lines.len(), 5);
    assert!(
        lines[1].contains("train:good") && lines[1].contains("correction"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].contains("none") && !lines[2].contains("would"),
        "{}",
        lines[2]
    );
    assert!(
        lines[3].contains("would move:Receipts") && lines[3].contains("(corrected by 4)"),
        "{}",
        lines[3]
    );
    assert!(
        lines[4].contains("move:Junk (undone)") && lines[4].contains("0.99"),
        "{}",
        lines[4]
    );

    let (_, out, _) = run(&["-c", &config, "log", "-n", "1"]).await;
    assert_eq!(out.lines().count(), 2);
}

#[tokio::test]
async fn log_shows_where_a_move_goes_and_marks_rule_decisions() {
    let dir = tempfile::tempdir().unwrap();
    let (config, store) = setup(dir.path(), "");
    store
        .record_decision(&decision("good", "move:Receipts", true, "<a@x>", "rule"))
        .unwrap();

    let (_, out, _) = run(&["-c", &config, "--color", "never", "log"]).await;
    assert!(
        out.lines().nth(1).unwrap().contains("INBOX → Receipts"),
        "{out}"
    );

    let (_, out, _) = run(&["-c", &config, "--color", "always", "log"]).await;
    assert!(out.contains("\x1b[34mrule"), "{out:?}");
}

#[tokio::test]
async fn doctor_summarises_the_configuration_and_what_has_been_learnt() {
    let dir = tempfile::tempdir().unwrap();
    let (config, store) = setup(dir.path(), "port = 1993\n[policy]\nmode = \"live\"\n");
    store
        .train("<t@x>", &["free".to_owned()], crate::classify::Label::Spam)
        .unwrap();

    let (_, out, _) = run(&["-c", &config, "--color", "never", "doctor"]).await;

    for want in [
        "me@example.org on imap.example.org",
        "live mode",
        "watching [INBOX], training TrainSpam, TrainGood",
        "0 addresses",
        "1 spam, 0 good",
        "1 tokens (1 seen once)",
        "not learnt; only the topmost header counts",
        "nothing saved yet; has 'tamis run' ever run?",
    ] {
        assert!(out.contains(want), "{want:?} not in\n{out}");
    }
}

#[tokio::test]
async fn doctor_prints_sections_and_fails_when_a_check_does() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = setup(dir.path(), "");

    let (code, out, _) = run(&["-c", &config, "--color", "never", "doctor"]).await;

    assert_eq!(code, 1, "an empty filter has not learnt both kinds of mail");
    assert!(
        out.contains("Configuration") && out.contains("Training") && out.contains("Decisions"),
        "{out}"
    );
    assert!(out.contains("FAIL  class balance"), "{out}");
    assert!(
        out.contains("database") && out.contains("0 tokens (0 seen once)"),
        "{out}"
    );

    let (code, out, _) = run(&["-c", &config, "doctor", "--json"]).await;
    assert_eq!(code, 1);
    let report: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(report["checks"][0]["name"], "config file");
}

#[tokio::test]
async fn doctor_reports_a_configuration_that_does_not_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[account]\nhost = \"\"\n").unwrap();

    let (code, out, _) = run(&["-c", path.to_str().unwrap(), "--color", "never", "doctor"]).await;

    assert_eq!(code, 1);
    assert!(
        out.contains("FAIL  config file") && out.contains("account.host is required"),
        "{out}"
    );
}

#[tokio::test]
async fn monitor_works_out_the_checks_to_create() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = setup(
        dir.path(),
        "[monitor]\nping_url = \"https://hc-ping.example.com/abc\"\n",
    );

    let (code, out, _) = run(&["-c", &config, "monitor"]).await;
    assert_eq!(code, 0);
    for want in [
        "tamis heartbeat",
        "2m",
        "11m",
        "monitor.ping_url",
        "monitor.quality_url (not set)",
        "the login is rejected",
        "at once",
        "healthchecks.io/api/v3/checks/",
        "\"unique\":[\"name\"]",
    ] {
        assert!(out.contains(want), "{want:?} not in\n{out}");
    }

    let (_, out, _) = run(&["-c", &config, "monitor", "--json"]).await;
    let settings: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(settings["heartbeat"]["timeout"], 120);
    assert_eq!(settings["alerts"][0]["afterSeconds"], 780);
}

#[tokio::test]
async fn monitor_test_pings_each_configured_check() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = setup(dir.path(), "");
    let (code, out, _) = run(&["-c", &config, "monitor", "test"]).await;
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            "No monitor.ping_url or monitor.quality_url configured.\n"
        )
    );

    let good = http_server(200).await;
    let bad = http_server(404).await;
    let (config, _) = setup(
        dir.path(),
        &format!("[monitor]\nping_url = \"{good}\"\nquality_url = \"{bad}\"\n"),
    );
    let (code, out, err) = run(&["-c", &config, "monitor", "test"]).await;

    assert_eq!(code, 1);
    assert!(
        out.contains("heartbeat: ok")
            && out.contains("quality: FAILED: unexpected response: HTTP 404"),
        "{out}"
    );
    assert!(err.contains("tamis: quality: unexpected response"), "{err}");
}

#[tokio::test]
async fn the_whitelist_can_be_edited_by_hand() {
    let dir = tempfile::tempdir().unwrap();
    let (config, store) = setup(dir.path(), "");
    let args = |rest: &[&'static str]| -> Vec<String> {
        ["-c", &config, "--color", "never"]
            .iter()
            .map(ToString::to_string)
            .chain(rest.iter().map(ToString::to_string))
            .collect()
    };
    let run = |rest: &[&'static str]| {
        let full = args(rest);
        async move {
            let refs: Vec<&str> = full.iter().map(String::as_str).collect();
            super::tests::run(&refs).await
        }
    };

    assert_eq!(
        run(&["whitelist", "list"]).await.1,
        "No addresses whitelisted.\n"
    );

    let (code, out, _) = run(&[
        "whitelist",
        "add",
        "Alice <Alice@Example.org>",
        "bob@example.org",
    ])
    .await;
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            "whitelisted alice@example.org\nwhitelisted bob@example.org\n"
        )
    );
    assert!(store.has_address("bob@example.org").unwrap());

    let (_, out, _) = run(&["whitelist", "list"]).await;
    assert!(
        out.starts_with("ADDRESS") && out.contains("alice@example.org") && out.contains("manual"),
        "{out}"
    );

    let (code, _, err) = run(&["whitelist", "add", "not an address"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("invalid address"), "{err}");

    let (code, _, err) = run(&["whitelist", "rm", "bob@example.org", "nobody@example.org"]).await;
    assert_eq!(code, 1);
    assert!(
        err.contains("nobody@example.org is not whitelisted"),
        "{err}"
    );
    assert!(
        store.has_address("bob@example.org").unwrap(),
        "nothing is removed unless every address is on the list"
    );

    let (_, out, _) = run(&["whitelist", "remove", "BOB@example.org"]).await;
    assert_eq!(out, "removed bob@example.org\n");

    let (_, out, _) = run(&["whitelist", "rm", "--matching", "EXAMPLE"]).await;
    assert!(
        out.contains("alice@example.org")
            && out.ends_with("1 address matches; run again with --yes to remove them.\n"),
        "{out}"
    );
    assert!(store.has_address("alice@example.org").unwrap());

    let (_, out, _) = run(&["whitelist", "rm", "--matching", "example", "--yes"]).await;
    assert_eq!(out, "removed 1 address\n");
    assert_eq!(
        run(&["whitelist", "rm", "--matching", "example"]).await.1,
        "No addresses match.\n"
    );

    for bad in [
        &["whitelist", "rm"][..],
        &["whitelist", "rm", "a@b.c", "--matching", "x"][..],
        &["whitelist", "add"][..],
    ] {
        assert_eq!(run(bad).await.0, 2, "{bad:?}");
    }
}

const SCRIPT: &str = "require \"fileinto\";\nif header :contains \"list-id\" \"golang\" { fileinto \"Lists/Go\"; }\n";

#[tokio::test]
async fn the_rules_can_be_shown_checked_and_tried() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("rules.sieve");
    std::fs::write(&script, SCRIPT).unwrap();
    let (config, _) = setup(
        dir.path(),
        &format!("[rules]\nscript = \"{}\"\n", script.display()),
    );
    let message = dir.path().join("m.eml");

    let (code, out, _) = run(&["-c", &config, "rules", "show"]).await;
    assert_eq!(code, 0);
    assert_eq!(out, format!("# {}\n{SCRIPT}", script.display()));

    let (code, out, _) = run(&["-c", &config, "rules", "check"]).await;
    assert_eq!((code, out), (0, format!("{}: ok\n", script.display())));

    std::fs::write(
        &message,
        "From: a@b.c\r\nList-Id: <golang-nuts.example>\r\n\r\nhi",
    )
    .unwrap();
    let (_, out, _) = run(&["-c", &config, "rules", "test", message.to_str().unwrap()]).await;
    assert_eq!(out, "filed into Lists/Go\n");

    std::fs::write(&message, "From: a@b.c\r\n\r\nhi").unwrap();
    let (_, out, _) = run(&["-c", &config, "rules", "test", message.to_str().unwrap()]).await;
    assert_eq!(out, "stays in the inbox\n");
}

#[tokio::test]
async fn rules_test_checks_messages_against_the_folder_their_header_names() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("rules.sieve");
    std::fs::write(&script, SCRIPT).unwrap();
    let go = dir.path().join("go.eml");
    let other = dir.path().join("other.eml");
    std::fs::write(
        &go,
        "From: a@b.c\r\nList-Id: <golang-nuts.example>\r\nX-Want: Lists/Go\r\n\r\nhi",
    )
    .unwrap();
    std::fs::write(&other, "From: a@b.c\r\nx-want: stay\r\n\r\nhi").unwrap();
    let (script, go, other) = (
        script.to_str().unwrap(),
        go.to_str().unwrap(),
        other.to_str().unwrap(),
    );

    // No configuration file is needed when --script names the rules.
    let (code, out, _) = run(&[
        "rules",
        "test",
        "--script",
        script,
        "--expect-header",
        "X-Want",
        go,
        other,
    ])
    .await;
    assert_eq!(
        (code, out),
        (0, format!("ok {go}\nok {other}\n2 passed, 0 failed\n"))
    );

    // Several messages without expectations are labelled with their path.
    let (code, out, _) = run(&["rules", "test", "--script", script, go, other]).await;
    assert_eq!(
        (code, out),
        (
            0,
            format!("{go}: filed into Lists/Go\n{other}: stays in the inbox\n")
        )
    );

    // A mismatch and a missing header both fail the run.
    std::fs::write(other, "From: a@b.c\r\nX-Want: Lists/Go\r\n\r\nhi").unwrap();
    let (code, out, err) = run(&[
        "rules",
        "test",
        "--script",
        script,
        "--expect-header",
        "X-Want",
        go,
        other,
    ])
    .await;
    assert_eq!(code, 1);
    assert!(
        out.contains(&format!("FAIL {other}: want \"Lists/Go\", got \"stay\"")),
        "{out}"
    );
    assert!(out.ends_with("1 passed, 1 failed\n"), "{out}");
    assert!(err.contains("1 of 2 message(s)"), "{err}");

    let (code, out, _) = run(&[
        "rules",
        "test",
        "--script",
        script,
        "--expect-header",
        "X-Nope",
        go,
    ])
    .await;
    assert_eq!(code, 1);
    assert!(
        out.contains("FAIL") && out.contains("no X-Nope header"),
        "{out}"
    );
}

#[tokio::test]
async fn a_script_that_does_not_compile_is_reported_before_the_configuration_is_needed() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.sieve");
    std::fs::write(&bad, "fileinto \"X\";").unwrap();

    let (code, _, err) = run(&["rules", "check", bad.to_str().unwrap()]).await;

    assert_eq!(code, 1);
    assert!(err.contains("bad.sieve: parse rules:"), "{err}");
}

#[tokio::test]
async fn rules_commands_without_a_script_say_so() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = setup(dir.path(), "");

    let (code, out, _) = run(&["-c", &config, "rules", "show"]).await;
    assert_eq!(
        (code, out.as_str()),
        (0, "No rules script configured (rules.script).\n")
    );

    let (code, _, err) = run(&["-c", &config, "rules", "check"]).await;
    assert_eq!(code, 1);
    assert!(
        err.contains("no rules script: set rules.script in the config file"),
        "{err}"
    );
}

const WEB: &str = "[web]\nenabled = true\norigin = \"https://tamis.example.org/\"\n";

#[tokio::test]
async fn the_web_commands_need_the_web_interface_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = setup(dir.path(), "");

    let (code, _, err) = run(&["-c", &config, "web", "enroll"]).await;

    assert_eq!(code, 1);
    assert!(err.contains("web is not enabled"), "{err}");
}

#[tokio::test]
async fn enroll_prints_a_link_that_works_once() {
    let dir = tempfile::tempdir().unwrap();
    let (config, store) = setup(dir.path(), WEB);

    let (code, out, _) = run(&["-c", &config, "web", "enroll"]).await;

    assert_eq!(code, 0, "{out}");
    let link = out.lines().last().unwrap();
    let token = link
        .strip_prefix("https://tamis.example.org/auth/enroll?token=")
        .unwrap();
    assert!(out.contains("within 15 minutes"), "{out}");
    assert!(
        store
            .consume_enrollment(&crate::web::hash_token(token), Timestamp::now())
            .unwrap()
    );
    assert!(
        !store
            .consume_enrollment(&crate::web::hash_token(token), Timestamp::now())
            .unwrap()
    );
}

#[tokio::test]
async fn passkeys_are_listed_and_revoked() {
    let dir = tempfile::tempdir().unwrap();
    let (config, store) = setup(dir.path(), WEB);
    let (_, none, _) = run(&["-c", &config, "web", "passkeys"]).await;
    store.add_passkey("abc", "laptop", b"{}").unwrap();
    let id = store.passkeys().unwrap()[0].id;

    let (code, listed, _) = run(&["-c", &config, "--color", "never", "web", "passkeys"]).await;
    let (_, revoked, _) = run(&["-c", &config, "web", "revoke", &id.to_string()]).await;

    assert!(none.contains("No passkeys enrolled"), "{none}");
    assert_eq!(code, 0);
    assert!(
        listed.contains("laptop") && listed.contains("never"),
        "{listed}"
    );
    assert_eq!(revoked, format!("revoked passkey {id}\n"));
    assert!(store.passkeys().unwrap().is_empty());
}

#[tokio::test]
async fn cert_shows_the_fingerprint_or_the_pem_and_needs_tls() {
    let dir = tempfile::tempdir().unwrap();
    let (plain, _) = setup(dir.path(), WEB);
    let (_, _, err) = run(&["-c", &plain, "web", "cert"]).await;
    assert!(err.contains("web is not serving TLS"), "{err}");

    let secure = tempfile::tempdir().unwrap();
    let (config, _) = setup(
        secure.path(),
        "[web]\nenabled = true\nself_signed = true\norigin = \"https://tamis.example.org\"\n",
    );
    let (code, out, _) = run(&["-c", &config, "web", "cert"]).await;
    let (_, pem, _) = run(&["-c", &config, "web", "cert", "--pem"]).await;

    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("SHA-256 fingerprint: ") && out.contains("web-cert.pem"),
        "{out}"
    );
    assert!(
        pem.starts_with("-----BEGIN CERTIFICATE-----") && !pem.contains("PRIVATE"),
        "{pem}"
    );
}
