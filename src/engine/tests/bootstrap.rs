use super::*;

fn corpus(env: &mut Env) {
    let old = jiff::Timestamp::from_second(1_000).unwrap();
    env.mail()
        .deliver_at("INBOX", &good("read-old"), &["\\Seen"], old);
    env.mail()
        .deliver_at("INBOX", &good("unread-old"), &[], old);
    env.mail().deliver("INBOX", &good("read-new"), &["\\Seen"]);
    env.mail().deliver("Junk", &spam("j1"), &[]);
    env.mail().deliver("Junk", &spam("j2"), &[]);
    env.mail().deliver(
        "Sent Messages",
        "From: me@example.org\r\nTo: bob@friends.example\r\nMessage-ID: <s1@x>\r\n\r\nhi",
        &[],
    );
}

fn options() -> BootstrapOptions {
    BootstrapOptions {
        sent_limit: 10,
        good: vec![Source {
            folder: "INBOX".into(),
            limit: 10,
        }],
        spam: vec![Source {
            folder: "Junk".into(),
            limit: 1,
        }],
        good_min_age: std::time::Duration::from_secs(3600),
        ..BootstrapOptions::default()
    }
}

#[tokio::test]
async fn bootstrap_learns_old_read_mail_and_the_latest_spam() {
    let mut env = Env::new(Mode::Live);
    corpus(&mut env);

    let summary = env.engine.bootstrap(&options()).await.unwrap();

    assert_eq!((summary.addresses, summary.good, summary.spam), (1, 1, 1));
    assert_eq!(
        env.store().trained("<read-old@x>").unwrap(),
        Some(Label::Good)
    );
    assert_eq!(
        env.store().trained("<unread-old@x>").unwrap(),
        None,
        "unread mail may be spam nobody has seen"
    );
    assert_eq!(
        env.store().trained("<read-new@x>").unwrap(),
        None,
        "recent mail is too new"
    );
    assert_eq!(
        env.store().trained("<j1@x>").unwrap(),
        None,
        "only the latest are taken"
    );
    assert_eq!(env.store().trained("<j2@x>").unwrap(), Some(Label::Spam));
    assert!(env.store().has_address("bob@friends.example").unwrap());
    assert_eq!(
        env.store().cursor("INBOX").unwrap().unwrap().last_uid,
        3,
        "watching starts from current mail"
    );
}

#[tokio::test]
async fn bootstrap_can_leave_the_cursors_alone() {
    let mut env = Env::new(Mode::Live);
    corpus(&mut env);

    env.engine
        .bootstrap(&BootstrapOptions {
            keep_cursors: true,
            ..options()
        })
        .await
        .unwrap();

    assert_eq!(env.store().cursor("INBOX").unwrap(), None);
}

#[tokio::test]
async fn refreshing_learns_what_was_learnt_again() {
    let mut env = Env::new(Mode::Live);
    corpus(&mut env);
    env.engine.bootstrap(&options()).await.unwrap();

    let again = env.engine.bootstrap(&options()).await.unwrap();
    assert_eq!(
        (again.good, again.spam, again.refreshed),
        (0, 0, 0),
        "nothing changed"
    );

    let refreshed = env
        .engine
        .bootstrap(&BootstrapOptions {
            refresh: true,
            ..options()
        })
        .await
        .unwrap();
    assert_eq!(
        (refreshed.good, refreshed.spam, refreshed.refreshed),
        (1, 1, 2)
    );
}

#[tokio::test]
async fn bootstrap_learns_the_providers_authserv_id() {
    let mut env = Env::new(Mode::Live);
    let old = jiff::Timestamp::from_second(1_000).unwrap();
    for n in 0..20 {
        let raw = message(
            &format!("a{n}"),
            "Alice <alice@friends.example>",
            "hi",
            "hello",
            "Authentication-Results: mx.icloud.com; dmarc=pass header.from=friends.example\r\n",
        );
        env.mail().deliver_at("INBOX", &raw, &["\\Seen"], old);
    }

    let wide = BootstrapOptions {
        good: vec![Source {
            folder: "INBOX".into(),
            limit: 50,
        }],
        ..options()
    };
    let summary = env.engine.bootstrap(&wide).await.unwrap();

    assert_eq!(summary.auth_server_id.as_deref(), Some("icloud.com"));
    let pin = resolve_auth_pin(env.store(), &env.engine.cfg).unwrap();
    assert_eq!(
        (pin.id.as_str(), pin.source),
        ("icloud.com", AuthSource::Learnt)
    );
}

#[tokio::test]
async fn a_failing_corpus_search_stops_bootstrap() {
    let mut env = Env::new(Mode::Live);
    env.mail()
        .fail("seen_before", MailError::Server("nope".into()));

    let err = env
        .engine
        .bootstrap(&options())
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("bootstrap from INBOX"), "{err}");
}
