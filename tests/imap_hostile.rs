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
