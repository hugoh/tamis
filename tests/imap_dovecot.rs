//! The real IMAP client against Dovecot. Needs `TAMIS_TEST_IMAP=host:port`;
//! `mise run test-imap` starts the container and sets it. Without it every
//! test returns at once.

#![allow(clippy::unwrap_used, reason = "a failing call should fail the test")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use tamis::classify::Label;
use tamis::config::{Account, Config, Mode};
use tamis::engine::Engine;
use tamis::mail::client::{Client, DialConfig};
use tamis::mail::watch::Watcher;
use tamis::mail::{Error, Mailbox};
use tamis::store::Store;

static NEXT_USER: AtomicU32 = AtomicU32::new(0);

fn server() -> Option<(String, u16)> {
    let address = std::env::var("TAMIS_TEST_IMAP").ok()?;
    let (host, port) = address.rsplit_once(':')?;
    Some((host.to_owned(), port.parse().ok()?))
}

fn user() -> String {
    format!(
        "user{}-{}",
        std::process::id(),
        NEXT_USER.fetch_add(1, Ordering::Relaxed)
    )
}

fn config(user: &str, password: &str) -> Option<DialConfig> {
    let (host, port) = server()?;
    Some(DialConfig {
        host,
        port,
        username: user.to_owned(),
        password: password.to_owned(),
        plaintext: true,
    })
}

/// A client for a fresh user, or `None` when no server is configured.
async fn connect() -> Option<(String, Client)> {
    let user = user();
    let client = Client::dial(&config(&user, "secret")?).await.unwrap();
    Some((user, client))
}

fn mail(id: &str, extra: &str) -> String {
    format!(
        "From: Alice <Alice@Example.org>\r\nTo: me@example.org, Bob <bob@example.org>\r\nCc: carol@example.org\r\nSubject: Hello {id}\r\nMessage-ID: <{id}@x>\r\n{extra}\r\nbody of {id}"
    )
}

#[tokio::test]
async fn a_wrong_password_is_a_login_error() {
    let Some(config) = config(&user(), "wrong") else {
        return;
    };

    let err = Client::dial(&config).await.unwrap_err();

    assert!(matches!(err, Error::Login(_)), "{err:?}");
}

#[tokio::test]
async fn an_unreachable_server_is_a_connection_error() {
    let Some(mut config) = config(&user(), "secret") else {
        return;
    };
    config.port = 1;

    assert!(Client::dial(&config).await.unwrap_err().is_connection());
}

#[tokio::test]
async fn status_search_and_fetch_read_the_folder() {
    let Some((_, mut client)) = connect().await else {
        return;
    };

    let empty = client.state("INBOX").await.unwrap();
    assert_eq!(empty.uid_next, 1);
    assert_eq!(client.messages("INBOX").await.unwrap(), 0);
    assert!(client.uids_after("INBOX", 0).await.unwrap().is_empty());

    for id in ["a", "b", "c"] {
        client
            .append("INBOX", mail(id, "").as_bytes(), &["$Custom"])
            .await
            .unwrap();
    }

    assert_eq!(client.state("INBOX").await.unwrap().uid_next, 4);
    assert_eq!(client.messages("INBOX").await.unwrap(), 3);
    assert_eq!(client.uids_after("INBOX", 1).await.unwrap(), [2, 3]);

    let fetched = client.fetch("INBOX", &[1, 3]).await.unwrap();
    assert_eq!(fetched.len(), 2);
    let first = &fetched[0];
    assert_eq!(
        (
            first.uid,
            first.message_id.as_str(),
            first.from.as_str(),
            first.subject.as_str()
        ),
        (1, "<a@x>", "alice@example.org", "Hello a")
    );
    assert!(first.raw.starts_with(b"From: Alice"));
    assert!(
        first.has_flag("$custom") && !first.has_flag("\\Seen"),
        "fetching does not mark mail as read: {:?}",
        first.flags
    );

    let envelopes = client.fetch_envelopes("INBOX", &[2]).await.unwrap();
    assert!(envelopes[0].raw.is_empty() && envelopes[0].subject == "Hello b");
    assert!(client.fetch("INBOX", &[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn encoded_subjects_are_decoded() {
    let Some((_, mut client)) = connect().await else {
        return;
    };
    let raw = "From: a@b.example\r\nSubject: =?UTF-8?Q?Th=C3=A9_au_lait?=\r\nMessage-ID: <enc@x>\r\n\r\nx";
    let uid = client
        .append("INBOX", raw.as_bytes(), &[])
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        client.fetch("INBOX", &[uid]).await.unwrap()[0].subject,
        "Thé au lait"
    );
}

#[tokio::test]
async fn message_ids_recipients_and_dates_are_searchable() {
    let Some((_, mut client)) = connect().await else {
        return;
    };
    client
        .append("INBOX", mail("abc", "").as_bytes(), &["\\Seen"])
        .await
        .unwrap();
    client
        .append("INBOX", mail("xyz", "").as_bytes(), &[])
        .await
        .unwrap();

    assert_eq!(
        client.find_message_id("INBOX", "<abc@x>").await.unwrap(),
        [1]
    );
    assert_eq!(
        client.find_message_id("INBOX", "b").await.unwrap(),
        [1],
        "a substring matches"
    );
    assert!(
        client
            .find_message_id("INBOX", "")
            .await
            .unwrap()
            .is_empty()
    );

    assert_eq!(
        client.recipients("INBOX", &[1, 2]).await.unwrap(),
        ["me@example.org", "bob@example.org", "carol@example.org"]
    );

    let tomorrow = Timestamp::now() + std::time::Duration::from_hours(48);
    assert_eq!(client.seen_before("INBOX", tomorrow).await.unwrap(), [1]);
    assert!(
        client
            .seen_before("INBOX", Timestamp::UNIX_EPOCH)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn keywords_can_be_set_cleared_and_searched_with_condstore() {
    let Some((_, mut client)) = connect().await else {
        return;
    };
    let uid = client
        .append("INBOX", mail("k", "").as_bytes(), &[])
        .await
        .unwrap()
        .unwrap();
    let before = client
        .mod_state("INBOX")
        .await
        .unwrap()
        .expect("Dovecot has CONDSTORE");
    let keywords = vec!["$Junk".to_owned(), "Junk".to_owned()];
    let long_ago = Timestamp::UNIX_EPOCH;

    client.add_keyword("INBOX", &[uid], "$Junk").await.unwrap();
    assert_eq!(
        client
            .search_keywords("INBOX", &keywords, long_ago, 0)
            .await
            .unwrap(),
        [uid]
    );
    assert!(client.fetch("INBOX", &[uid]).await.unwrap()[0].has_flag("$junk"));

    let after = client.mod_state("INBOX").await.unwrap().unwrap();
    assert!(after.highest_mod_seq > before.highest_mod_seq);
    assert_eq!(
        client
            .search_keywords("INBOX", &keywords, long_ago, before.highest_mod_seq)
            .await
            .unwrap(),
        [uid]
    );
    assert!(
        client
            .search_keywords("INBOX", &keywords, long_ago, after.highest_mod_seq)
            .await
            .unwrap()
            .is_empty()
    );

    client
        .remove_keyword("INBOX", &[uid], "$Junk")
        .await
        .unwrap();
    assert!(
        client
            .search_keywords("INBOX", &keywords, long_ago, 0)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn folders_can_be_listed_and_created() {
    let Some((_, mut client)) = connect().await else {
        return;
    };

    assert!(client.exists("Junk").await.unwrap());
    assert!(!client.exists("Lists/Go").await.unwrap());
    client.ensure("Lists/Go").await.unwrap();
    client.ensure("Lists/Go").await.unwrap();
    assert!(client.exists("Lists/Go").await.unwrap());

    let folders = client.folders().await.unwrap();
    assert!(
        folders.iter().any(|f| f == "INBOX") && folders.iter().any(|f| f == "Lists/Go"),
        "{folders:?}"
    );
}

async fn moving_keeps_flags_and_is_repeatable(avoid_move: bool) {
    let Some((_, mut client)) = connect().await else {
        return;
    };
    if avoid_move {
        client.avoid_move();
    }
    let kept = client
        .append("INBOX", mail("m1", "").as_bytes(), &["$Keep"])
        .await
        .unwrap()
        .unwrap();
    let other = client
        .append("INBOX", mail("m2", "").as_bytes(), &[])
        .await
        .unwrap()
        .unwrap();
    // A message another actor flagged \Deleted must survive our expunge.
    client
        .add_keyword("INBOX", &[other], "\\Deleted")
        .await
        .unwrap();

    client
        .move_messages("INBOX", &[kept], "Junk")
        .await
        .unwrap();
    client
        .move_messages("INBOX", &[kept], "Junk")
        .await
        .unwrap();

    assert_eq!(
        client.uids_after("INBOX", 0).await.unwrap(),
        [other],
        "only the moved message left"
    );
    let in_junk = client_uids(&mut client, "Junk").await;
    let moved = client.fetch("Junk", &in_junk).await.unwrap();
    assert_eq!(moved.len(), 1, "moving again does not copy twice");
    assert!(
        moved[0].has_flag("$keep") && moved[0].message_id == "<m1@x>",
        "{:?}",
        moved[0]
    );
}

async fn client_uids(client: &mut Client, folder: &str) -> Vec<u32> {
    client.uids_after(folder, 0).await.unwrap()
}

#[tokio::test]
async fn moving_uses_move() {
    moving_keeps_flags_and_is_repeatable(false).await;
}

#[tokio::test]
async fn moving_falls_back_to_copy_and_uid_expunge() {
    moving_keeps_flags_and_is_repeatable(true).await;
}

#[tokio::test]
async fn a_missing_folder_is_a_server_error_not_a_dropped_connection() {
    let Some((_, mut client)) = connect().await else {
        return;
    };

    let err = client.state("No/Such/Folder").await.unwrap_err();

    assert!(matches!(err, Error::Server(_)), "{err:?}");
    assert_eq!(
        client.messages("INBOX").await.unwrap(),
        0,
        "the connection still works"
    );
}

#[tokio::test]
async fn the_engine_runs_a_pass_against_a_real_server() {
    let Some((user, mut seeder)) = connect().await else {
        return;
    };
    let (host, _) = server().unwrap();
    let engine_client = Client::dial(&config(&user, "secret").unwrap())
        .await
        .unwrap();

    let spam = |id: &str| {
        format!(
            "From: Prize <winner@lottery.example>\r\nTo: me@example.org\r\nSubject: Cheap pills winner\r\nMessage-ID: <{id}@x>\r\n\r\nfree prize cheap pills winner claim"
        )
    };
    let good = |id: &str| {
        format!(
            "From: Alice <alice@friends.example>\r\nTo: me@example.org\r\nSubject: Lunch on Thursday\r\nMessage-ID: <{id}@x>\r\n\r\nlunch thursday meeting agenda"
        )
    };

    // Teach it through the training folders, which the engine also moves on.
    for folder in ["TrainSpam", "TrainGood"] {
        seeder.ensure(folder).await.unwrap();
    }
    for n in 0..5 {
        seeder
            .append("TrainSpam", spam(&format!("ts{n}")).as_bytes(), &[])
            .await
            .unwrap();
        seeder
            .append("TrainGood", good(&format!("tg{n}")).as_bytes(), &[])
            .await
            .unwrap();
    }

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("state.db")).unwrap());
    let mut cfg = Config {
        account: Account {
            host,
            user,
            ..Account::default()
        },
        ..Config::default()
    };
    cfg.policy.mode = Mode::Live;
    let mut engine = Engine::new(engine_client, Arc::clone(&store), Arc::new(cfg));

    let first = engine.run_once().await.unwrap();
    assert_eq!(first.trained, 10);
    assert_eq!(store.trained("<ts0@x>").unwrap(), Some(Label::Spam));
    assert_eq!(seeder.messages("Junk").await.unwrap(), 5);
    assert_eq!(seeder.messages("INBOX").await.unwrap(), 5);

    let uid = seeder
        .append("INBOX", spam("new").as_bytes(), &[])
        .await
        .unwrap()
        .unwrap();
    let second = engine.run_once().await.unwrap();

    assert_eq!((second.scanned, second.spam), (1, 1));
    assert!(
        seeder
            .uids_after("INBOX", uid - 1)
            .await
            .unwrap()
            .is_empty(),
        "the spam left the inbox"
    );
    let junk = seeder.uids_after("Junk", 5).await.unwrap();
    let filed = &seeder.fetch("Junk", &junk).await.unwrap()[0];
    assert!(
        filed.has_flag("$tamisfiled") && filed.message_id == "<new@x>",
        "{filed:?}"
    );
}

async fn wait_until_idling(watcher: &Watcher, folders: usize) {
    for _ in 0..100 {
        if watcher.idling() == folders {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the watcher never started idling on {folders} folders");
}

/// Swallows what has already been reported, so the next wait is for news.
async fn drain(watcher: &Watcher) {
    while tokio::time::timeout(Duration::from_millis(300), watcher.changed())
        .await
        .is_ok()
    {}
}

#[tokio::test]
async fn a_watcher_hears_of_new_mail_and_flag_changes() {
    let Some((user, mut writer)) = connect().await else {
        return;
    };
    let watcher = Watcher::start(
        &config(&user, "secret").unwrap(),
        &["INBOX".to_owned()],
        Duration::from_millis(200),
    );
    wait_until_idling(&watcher, 1).await;
    drain(&watcher).await;

    let uid = writer
        .append("INBOX", mail("w1", "").as_bytes(), &[])
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), watcher.changed())
        .await
        .expect("new mail was not reported");

    wait_until_idling(&watcher, 1).await;
    drain(&watcher).await;
    writer.add_keyword("INBOX", &[uid], "$Junk").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), watcher.changed())
        .await
        .expect("a flag change was not reported");

    tokio::time::timeout(Duration::from_secs(5), watcher.close())
        .await
        .expect("close hung");
}

#[tokio::test]
async fn a_watcher_closes_promptly_while_idle() {
    let Some((user, _)) = connect().await else {
        return;
    };
    let folders = ["INBOX".to_owned(), "Junk".to_owned()];
    let watcher = Watcher::start(
        &config(&user, "secret").unwrap(),
        &folders,
        Duration::from_millis(200),
    );
    wait_until_idling(&watcher, 2).await;

    tokio::time::timeout(Duration::from_secs(5), watcher.close())
        .await
        .expect("close hung");
}
