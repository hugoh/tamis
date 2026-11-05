//! The command line against Dovecot, through the same entry point the binary
//! uses. Needs `TAMIS_TEST_IMAP=host:port`; `mise run test-imap` sets it.

#![allow(clippy::unwrap_used, reason = "a failing call should fail the test")]

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tamis::cli::{self, Env};
use tamis::mail::Mailbox;
use tamis::mail::client::{Client, DialConfig};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn server() -> Option<(String, u16)> {
    let address = std::env::var("TAMIS_TEST_IMAP").ok()?;
    let (host, port) = address.rsplit_once(':')?;
    Some((host.to_owned(), port.parse().ok()?))
}

fn user() -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    format!(
        "cli{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

struct Account {
    config: String,
    dial: DialConfig,
}

fn account(dir: &Path, mode: &str, extra: &str) -> Option<Account> {
    let (host, port) = server()?;
    let user = user();
    let state = dir.join("state.db");
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "state = \"{}\"\n[account]\nhost = \"{host}\"\nport = {port}\nuser = \"{user}\"\n[policy]\nmode = \"{mode}\"\npoll_interval = \"1s\"\n{extra}",
            state.display()
        ),
    )
    .unwrap();
    let dial = DialConfig {
        host,
        port,
        username: user,
        password: "secret".into(),
        plaintext: true,
    };
    Some(Account {
        config: config.to_string_lossy().into_owned(),
        dial,
    })
}

fn env(stdout: &Capture, stderr: &Capture) -> Env {
    Env {
        stdin: Box::new(std::io::empty().into_buf()),
        stdout: Box::new(stdout.clone()),
        stderr: Box::new(stderr.clone()),
        getenv: Arc::new(|key| (key == "TAMIS_PASSWORD").then(|| "secret".to_owned())),
        stdout_is_terminal: false,
        prompt: None,
        keyring: None,
        plaintext_imap: true,
        version: "test".to_owned(),
        logging: None,
    }
}

trait IntoBuf {
    fn into_buf(self) -> std::io::BufReader<std::io::Empty>;
}

impl IntoBuf for std::io::Empty {
    fn into_buf(self) -> std::io::BufReader<std::io::Empty> {
        std::io::BufReader::new(self)
    }
}

async fn tamis(config: &str, command: &[&str]) -> (i32, String, String) {
    let (out, err) = (Capture::default(), Capture::default());
    let mut line = vec!["-c".to_owned(), config.to_owned()];
    line.extend(command.iter().map(|a| (*a).to_owned()));
    let code = cli::run(&line, env(&out, &err), CancellationToken::new()).await;
    (code, out.text(), err.text())
}

fn spam(id: &str) -> String {
    format!(
        "From: Prize <winner@lottery.example>\r\nTo: me@example.org\r\nSubject: Cheap pills winner\r\nMessage-ID: <{id}@x>\r\n\r\nfree prize cheap pills winner claim"
    )
}

fn good(id: &str) -> String {
    format!(
        "From: Alice <alice@friends.example>\r\nTo: me@example.org\r\nSubject: Lunch on Thursday\r\nMessage-ID: <{id}@x>\r\n\r\nlunch thursday meeting agenda"
    )
}

/// Teaches the filter through the training folders and a first pass.
async fn teach(account: &Account, client: &mut Client) {
    for folder in ["TrainSpam", "TrainGood"] {
        client.ensure(folder).await.unwrap();
    }
    for n in 0..5 {
        client
            .append("TrainSpam", spam(&format!("ts{n}")).as_bytes(), &[])
            .await
            .unwrap();
        client
            .append("TrainGood", good(&format!("tg{n}")).as_bytes(), &[])
            .await
            .unwrap();
    }
    let (code, _, err) = tamis(&account.config, &["run", "--once"]).await;
    assert_eq!((code, err.as_str()), (0, ""));
}

#[tokio::test]
async fn bootstrap_seeds_the_filter_from_the_mailbox() {
    let dir = tempfile::tempdir().unwrap();
    let Some(account) = account(dir.path(), "shadow", "") else {
        return;
    };
    let mut client = Client::dial(&account.dial).await.unwrap();
    let old = "Date: Mon, 01 Jan 2024 10:00:00 +0000\r\n";
    for n in 0..3 {
        client
            .append("Junk", spam(&format!("j{n}")).as_bytes(), &[])
            .await
            .unwrap();
        client
            .append(
                "INBOX",
                format!("{old}{}", good(&format!("g{n}"))).as_bytes(),
                &["\\Seen"],
            )
            .await
            .unwrap();
    }
    client
        .append(
            "Sent Messages",
            "From: me@example.org\r\nTo: bob@friends.example\r\nMessage-ID: <s@x>\r\n\r\nhi"
                .as_bytes(),
            &[],
        )
        .await
        .unwrap();

    let (code, out, err) = tamis(&account.config, &["bootstrap", "--good-min-age", "0s"]).await;

    assert_eq!((code, err.as_str()), (0, ""));
    assert!(
        out.contains("Whitelisted 1 addresses from Sent Messages."),
        "{out}"
    );
    // Read mail has to arrive before today to count as good, and these
    // messages arrived just now.
    assert!(out.contains("Trained 3 spam and 0 good messages."), "{out}");
    assert!(
        out.contains("Watching [INBOX] from now on, in shadow mode. Next: tamis run"),
        "{out}"
    );

    let (_, out, _) = tamis(&account.config, &["whitelist", "list"]).await;
    assert!(out.contains("bob@friends.example"), "{out}");
    let (_, out, _) = tamis(&account.config, &["status"]).await;
    assert!(out.contains("3 spam, 0 good messages"), "{out}");
}

#[tokio::test]
async fn one_pass_files_spam_and_undo_brings_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let Some(account) = account(dir.path(), "live", "") else {
        return;
    };
    let mut client = Client::dial(&account.dial).await.unwrap();
    teach(&account, &mut client).await;
    client
        .append("INBOX", spam("new").as_bytes(), &[])
        .await
        .unwrap();

    let (code, _, err) = tamis(&account.config, &["run", "--once"]).await;
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        client.messages("INBOX").await.unwrap(),
        5,
        "the spam left the inbox"
    );

    let (_, log, _) = tamis(&account.config, &["--color", "never", "log", "-n", "1"]).await;
    assert!(log.contains("spam") && log.contains("move:Junk"), "{log}");
    let id = log
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();

    let (code, out, err) = tamis(&account.config, &["undo", &id]).await;
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        out,
        format!("{id}: moved back to INBOX, learnt as good, sender whitelisted\n")
    );
    assert_eq!(client.messages("INBOX").await.unwrap(), 6);

    let (code, _, err) = tamis(&account.config, &["undo", &id]).await;
    assert_eq!(code, 1);
    assert!(err.contains("already undone"), "{err}");
}

#[tokio::test]
async fn learn_teaches_what_shadow_mode_only_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let Some(account) = account(dir.path(), "shadow", "") else {
        return;
    };
    let mut client = Client::dial(&account.dial).await.unwrap();
    teach(&account, &mut client).await;
    client
        .append("INBOX", spam("new").as_bytes(), &[])
        .await
        .unwrap();
    tamis(&account.config, &["run", "--once"]).await;
    let (_, log, _) = tamis(&account.config, &["--color", "never", "log", "-n", "1"]).await;
    let id = log
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();

    let (code, out, err) = tamis(&account.config, &["learn", &id, "--good"]).await;

    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(out, format!("{id}: learnt as good, sender whitelisted\n"));
    let (code, _, err) = tamis(&account.config, &["learn", &id, "--good", "--spam"]).await;
    assert_eq!(code, 2, "{err}");
}

#[tokio::test]
async fn rules_check_finds_the_folders_a_script_files_into() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("rules.sieve");
    std::fs::write(&script, "require \"fileinto\";\nif true { fileinto \"Junkk\"; fileinto \"Lists\"; fileinto \"Nowhere\"; }\n").unwrap();
    let Some(account) = account(
        dir.path(),
        "shadow",
        &format!("[rules]\nscript = \"{}\"\n", script.display()),
    ) else {
        return;
    };
    let mut client = Client::dial(&account.dial).await.unwrap();
    client.ensure("Lists").await.unwrap();

    let (code, out, err) = tamis(&account.config, &["rules", "check", "--folders"]).await;

    assert_eq!(code, 1);
    assert!(
        out.contains("no such folder \"Junkk\" (did you mean \"Junk\"?)"),
        "{out}"
    );
    assert!(out.contains("no such folder \"Nowhere\"\n"), "{out}");
    assert!(
        err.contains("rules file into folders that don't exist: 2 folder(s)"),
        "{err}"
    );

    client.ensure("Junkk").await.unwrap();
    client.ensure("Nowhere").await.unwrap();
    let (code, out, _) = tamis(&account.config, &["rules", "check", "--folders"]).await;
    assert_eq!(code, 0);
    assert!(out.ends_with("all 3 folders exist\n"), "{out}");
}

#[tokio::test]
async fn the_daemon_passes_when_mail_arrives_and_stops_on_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let Some(account) = account(dir.path(), "live", "") else {
        return;
    };
    let mut client = Client::dial(&account.dial).await.unwrap();
    teach(&account, &mut client).await;

    let shutdown = CancellationToken::new();
    let (out, err) = (Capture::default(), Capture::default());
    let argv = vec!["-c".to_owned(), account.config.clone(), "run".to_owned()];
    let running = tokio::spawn({
        let (env, shutdown) = (env(&out, &err), shutdown.clone());
        async move { cli::run(&argv, env, shutdown).await }
    });

    // The first pass starts at the current mail; mail after it is judged.
    tokio::time::sleep(Duration::from_secs(2)).await;
    client
        .append("INBOX", spam("arrives").as_bytes(), &[])
        .await
        .unwrap();
    for _ in 0..100 {
        if client.messages("INBOX").await.unwrap() == 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        client.messages("INBOX").await.unwrap(),
        5,
        "the daemon filed the spam"
    );

    shutdown.cancel();
    let code = tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("run did not stop")
        .unwrap();
    assert_eq!(code, 0);

    let (_, status, _) = tamis(&account.config, &["status"]).await;
    assert!(
        status.contains("last pass") || status.contains("running since"),
        "{status}"
    );
}

#[tokio::test]
async fn probe_reports_the_server_and_leaves_no_scratch_folders() {
    let dir = tempfile::tempdir().unwrap();
    let Some(account) = account(dir.path(), "shadow", "") else {
        return;
    };

    let (code, out, err) = tamis(&account.config, &["probe", "--write", "--idle-wait", "5s"]).await;

    assert_eq!(code, 0, "{out}\n{err}");
    for expected in [
        "ok    login",
        "ok    IDLE",
        "ok    APPEND",
        "ok    BODY.PEEK",
        "ok    STORE keyword",
        "ok    IDLE push",
        "ok    MOVE message",
        "ok    DELETE scratch",
    ] {
        assert!(
            out.lines().any(|line| line.starts_with(expected)),
            "missing {expected:?} in:\n{out}"
        );
    }
    let mut client = Client::dial(&account.dial).await.unwrap();
    let leftovers = client.list_with_attributes().await.unwrap();
    assert!(
        leftovers
            .iter()
            .all(|(name, _)| !name.starts_with("tamis-probe-")),
        "{leftovers:?}"
    );
}

#[tokio::test]
async fn probe_without_write_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let Some(account) = account(dir.path(), "shadow", "") else {
        return;
    };

    let (code, out, _) = tamis(&account.config, &["probe", "--json"]).await;

    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(r#""name": "write checks""#) && out.contains(r#""status": "skip""#),
        "{out}"
    );
}

#[tokio::test]
async fn run_serves_the_web_interface_and_stops_it_on_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let web = format!(
        "[web]\nenabled = true\nlisten = \"127.0.0.1:{port}\"\norigin = \"http://localhost:{port}\"\n"
    );
    let Some(account) = account(dir.path(), "shadow", &web) else {
        return;
    };

    let shutdown = CancellationToken::new();
    let (out, err) = (Capture::default(), Capture::default());
    let argv = vec!["-c".to_owned(), account.config.clone(), "run".to_owned()];
    let running = tokio::spawn({
        let (env, shutdown) = (env(&out, &err), shutdown.clone());
        async move { cli::run(&argv, env, shutdown).await }
    });

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let mut login = None;
    for _ in 0..50 {
        if let Ok(response) = client
            .get(format!("http://127.0.0.1:{port}/auth/login"))
            .send()
            .await
        {
            login = Some(response);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let login = login.expect("the web interface did not come up");
    let home = client
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .unwrap();

    assert_eq!(login.status(), 200);
    assert_eq!(home.status(), 303);
    assert!(
        login
            .text()
            .await
            .unwrap()
            .contains("Sign in with a passkey")
    );

    shutdown.cancel();
    let code = tokio::time::timeout(Duration::from_secs(15), running)
        .await
        .expect("run did not stop")
        .unwrap();
    assert_eq!((code, err.text()), (0, String::new()));
    assert!(
        client
            .get(format!("http://127.0.0.1:{port}/auth/login"))
            .send()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn run_serves_https_with_a_self_signed_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let web = format!(
        "[web]\nenabled = true\nself_signed = true\nlisten = \"127.0.0.1:{port}\"\norigin = \"https://localhost:{port}\"\n"
    );
    let Some(account) = account(dir.path(), "shadow", &web) else {
        return;
    };

    let shutdown = CancellationToken::new();
    let (out, err) = (Capture::default(), Capture::default());
    let argv = vec!["-c".to_owned(), account.config.clone(), "run".to_owned()];
    let running = tokio::spawn({
        let (env, shutdown) = (env(&out, &err), shutdown.clone());
        async move { cli::run(&argv, env, shutdown).await }
    });

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let mut login = None;
    for _ in 0..50 {
        if let Ok(response) = client
            .get(format!("https://localhost:{port}/auth/login"))
            .send()
            .await
        {
            login = Some(response);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let login = login.expect("the web interface did not come up over HTTPS");

    assert_eq!(login.status(), 200);
    assert!(dir.path().join("web-cert.pem").exists());

    shutdown.cancel();
    let code = tokio::time::timeout(Duration::from_secs(15), running)
        .await
        .expect("run did not stop")
        .unwrap();
    assert_eq!(code, 0, "{}", err.text());
}
