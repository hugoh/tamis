//! A server that sends what some real ones do and parsers choke on: VANISHED
//! (a QRESYNC response nobody asked for), and untagged lines no client knows.
//! The client must answer or fail, never hang: a hang stalls the whole pass.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failing call should fail the test"
)]

use std::time::Duration;

use tamis::mail::client::{Client, DialConfig};
use tamis::mail::{Error, Mailbox};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::time::timeout;

/// Serves one connection: logs in, and answers `STATUS` with `noise` first.
async fn serve(listener: TcpListener, noise: &'static str) {
    let (stream, _) = listener.accept().await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    write
        .write_all(b"* OK [CAPABILITY IMAP4rev1 IDLE UIDPLUS MOVE] ready\r\n")
        .await
        .unwrap();
    while let Ok(Some(line)) = lines.next_line().await {
        let (tag, command) = line.split_once(' ').unwrap_or((&line, ""));
        let reply = if command.starts_with("CAPABILITY") {
            format!("* CAPABILITY IMAP4rev1 IDLE UIDPLUS MOVE\r\n{tag} OK done\r\n")
        } else if command.starts_with("STATUS") {
            format!("{noise}* STATUS INBOX (UIDNEXT 7 UIDVALIDITY 3)\r\n{tag} OK done\r\n")
        } else if command.starts_with("LOGOUT") {
            format!("* BYE bye\r\n{tag} OK done\r\n")
        } else {
            format!("{tag} OK done\r\n")
        };
        write.write_all(reply.as_bytes()).await.unwrap();
    }
}

async fn state_with(noise: &'static str) -> std::result::Result<tamis::mail::State, Error> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(serve(listener, noise));

    let config = DialConfig {
        host: "127.0.0.1".to_owned(),
        port,
        username: "u".to_owned(),
        password: "p".to_owned(),
        plaintext: true,
    };
    let mut client = timeout(Duration::from_secs(10), Client::dial(&config))
        .await
        .expect("dialling hung")?;
    timeout(Duration::from_secs(10), client.state("INBOX"))
        .await
        .expect("the command hung")
}

#[tokio::test]
async fn a_well_behaved_server_works() {
    let state = state_with("").await.unwrap();

    assert_eq!((state.uid_validity, state.uid_next), (3, 7));
}

#[tokio::test]
async fn unrequested_vanished_is_ignored() {
    let state = state_with("* VANISHED (EARLIER) 1:5\r\n").await.unwrap();

    assert_eq!(state.uid_next, 7);
}

#[tokio::test]
async fn lines_no_client_understands_fail_the_connection_instead_of_hanging_it() {
    for (name, noise) in [
        ("unknown data", "* XYZZY something odd\r\n"),
        ("garbage", "\u{1}\u{2} not imap at all\r\n"),
    ] {
        let err = state_with(noise).await.unwrap_err();

        assert!(err.is_connection(), "{name}: {err:?}");
    }
}

/// Serves one connection with the given capabilities and `LOGIN` answer.
async fn serve_login(listener: TcpListener, capabilities: &'static str, login: &'static str) {
    let (stream, _) = listener.accept().await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    write
        .write_all(format!("* OK [CAPABILITY {capabilities}] ready\r\n").as_bytes())
        .await
        .unwrap();
    while let Ok(Some(line)) = lines.next_line().await {
        let (tag, command) = line.split_once(' ').unwrap_or((&line, ""));
        let reply = if command.starts_with("CAPABILITY") {
            format!("* CAPABILITY {capabilities}\r\n{tag} OK done\r\n")
        } else if command.starts_with("LOGIN") {
            format!("{tag} {login}\r\n")
        } else {
            format!("{tag} OK done\r\n")
        };
        write.write_all(reply.as_bytes()).await.unwrap();
    }
}

async fn watcher_for(
    capabilities: &'static str,
    login: &'static str,
) -> tamis::mail::watch::Watcher {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(serve_login(listener, capabilities, login));

    let config = DialConfig {
        host: "127.0.0.1".to_owned(),
        port,
        username: "u".to_owned(),
        password: "p".to_owned(),
        plaintext: true,
    };
    tamis::mail::watch::Watcher::start(&config, &["INBOX".to_owned()], Duration::from_millis(50))
}

#[tokio::test]
async fn a_server_without_idle_is_given_up_on() {
    let watcher = watcher_for("IMAP4rev1 UIDPLUS MOVE", "OK logged in").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(watcher.idling(), 0);
    timeout(Duration::from_secs(5), watcher.close())
        .await
        .expect("close hung");
}

#[tokio::test]
async fn a_rejected_login_is_retried_rarely_and_does_not_block_closing() {
    let watcher = watcher_for("IMAP4rev1 IDLE", "NO [AUTHENTICATIONFAILED] bad password").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(watcher.idling(), 0);
    timeout(Duration::from_secs(5), watcher.close())
        .await
        .expect("close hung");
}

/// Serves one connection that accepts the login and returns the `LOGIN` line
/// it was sent, so a test can see how credentials are encoded.
async fn login_line_for(username: &str, password: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let mut login = String::new();
        write
            .write_all(b"* OK [CAPABILITY IMAP4rev1 LITERAL+] ready\r\n")
            .await
            .unwrap();
        while let Ok(Some(line)) = lines.next_line().await {
            let (tag, command) = line.split_once(' ').unwrap_or((&line, ""));
            if command.starts_with("LOGIN") {
                login = line.clone();
            }
            let reply = if command.starts_with("CAPABILITY") {
                format!("* CAPABILITY IMAP4rev1 LITERAL+\r\n{tag} OK done\r\n")
            } else if command.starts_with("LOGIN") {
                format!("{tag} OK [CAPABILITY IMAP4rev1 LITERAL+] logged in\r\n")
            } else {
                format!("{tag} OK done\r\n")
            };
            write.write_all(reply.as_bytes()).await.unwrap();
            if command.starts_with("LOGOUT") {
                break;
            }
        }
        login
    });

    let config = DialConfig {
        host: "127.0.0.1".to_owned(),
        port,
        username: username.to_owned(),
        password: password.to_owned(),
        plaintext: true,
    };
    let client = Client::dial(&config).await.unwrap();
    client.close().await;
    seen.await.unwrap()
}

#[tokio::test]
async fn credentials_reach_the_server_as_typed() {
    let cases = [
        (
            "hugoh@hugoh.net",
            "abcd-efgh-ijkl-mnop",
            r#"LOGIN "hugoh@hugoh.net" "abcd-efgh-ijkl-mnop""#,
        ),
        (
            "me@example.org",
            "has space",
            r#"LOGIN "me@example.org" "has space""#,
        ),
        (
            "me@example.org",
            r#"quo"te\slash"#,
            r#"LOGIN "me@example.org" "quo\"te\\slash""#,
        ),
    ];
    for (user, password, want) in cases {
        let line = login_line_for(user, password).await;

        let command = line.split_once(' ').map_or("", |(_, rest)| rest);
        assert_eq!(command, want, "password {password:?}");
    }
}
