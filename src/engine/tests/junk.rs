use super::*;

#[tokio::test]
async fn mail_the_user_moves_to_junk_is_learnt_as_missed_spam() {
    let mut env = Env::trained();
    env.store()
        .add_address("zed@nowhere.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver("INBOX", &unknown("m1"), &[]);
    env.engine.run_once().await.unwrap();
    let uid = env.mail().uids("INBOX")[0];

    env.mail()
        .move_messages("INBOX", &[uid], "Junk")
        .await
        .unwrap();
    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.corrected, 1);
    assert_eq!(env.store().trained("<m1@x>").unwrap(), Some(Label::Spam));
    assert!(
        !env.store().has_address("zed@nowhere.example").unwrap(),
        "the sender comes off the whitelist"
    );
    assert_eq!(env.decisions()[0].reason, "correction");
}

#[tokio::test]
async fn spam_tamis_filed_is_not_a_correction_when_it_reaches_junk() {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("INBOX", &spam("s1"), &[]);
    env.engine.run_once().await.unwrap();
    let before = env.decisions().len();

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(
        (summary.corrected, summary.trained, summary.rescued),
        (0, 0, 0)
    );
    assert_eq!(env.decisions().len(), before);
}

#[tokio::test]
async fn a_rescue_moved_back_to_junk_is_learnt_as_spam() {
    let mut env = Env::trained();
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver(
        "Junk",
        &authenticated("r1", "Alice <alice@friends.example>"),
        &[],
    );
    env.engine.run_once().await.unwrap();
    assert_eq!(env.mail().uids("INBOX").len(), 1, "rescued");

    let uid = env.mail().uids("INBOX")[0];
    env.mail()
        .move_messages("INBOX", &[uid], "Junk")
        .await
        .unwrap();
    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.corrected, 1);
    assert_eq!(env.store().trained("<r1@x>").unwrap(), Some(Label::Spam));
    assert!(!env.store().has_address("alice@friends.example").unwrap());
}

#[tokio::test]
async fn whitelisted_authenticated_mail_is_rescued_from_junk() {
    let mut env = Env::trained();
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver(
        "Junk",
        &authenticated("r1", "Alice <alice@friends.example>"),
        &[],
    );

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.rescued, 1);
    assert!(env.mail().uids("Junk").is_empty());
    assert_eq!(
        env.mail().flags("INBOX", 1),
        Some(vec!["$tamisrescued".to_owned()])
    );
    let decision = &env.decisions()[0];
    assert_eq!(
        (
            decision.reason.as_str(),
            decision.action.to_string().as_str(),
            decision.applied
        ),
        ("whitelisted", "move:INBOX", true)
    );

    assert_eq!(
        env.engine.run_once().await.unwrap().scanned,
        0,
        "a rescue is not judged again in the inbox"
    );
}

#[tokio::test]
async fn rescue_needs_authentication_even_when_the_whitelist_does_not() {
    let mut env = Env::trained_with(Mode::Live, |cfg| cfg.policy.whitelist_requires_auth = false);
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver(
        "Junk",
        &message("r1", "Alice <alice@friends.example>", "Hello", "hello", ""),
        &[],
    );

    assert_eq!(env.engine.run_once().await.unwrap().rescued, 0);
}

#[tokio::test]
async fn rescue_only_reports_in_shadow_mode_and_can_go_to_its_own_folder() {
    let mut env = Env::trained_with(Mode::Shadow, |_| {});
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver(
        "Junk",
        &authenticated("r1", "Alice <alice@friends.example>"),
        &[],
    );
    env.engine.run_once().await.unwrap();
    assert_eq!(env.mail().uids("Junk"), [1]);
    assert!(!env.decisions()[0].applied);

    let mut env = Env::trained_with(Mode::Live, |cfg| cfg.folders.rescued = "Rescued".to_owned());
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver(
        "Junk",
        &authenticated("r1", "Alice <alice@friends.example>"),
        &[],
    );
    env.engine.run_once().await.unwrap();
    assert_eq!(env.mail().uids("Rescued"), [1], "the folder is created");
}

#[tokio::test]
async fn a_very_low_score_rescues_when_a_threshold_is_set() {
    let mut env = Env::trained_with(Mode::Live, |cfg| cfg.policy.rescue_threshold = 0.1);
    env.start().await;
    env.mail().deliver("Junk", &good("g1"), &[]);
    env.mail().deliver("Junk", &unknown("u1"), &[]);

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.rescued, 1);
    assert_eq!(env.mail().uids("Junk").len(), 1);
    assert_eq!(
        env.decisions()
            .iter()
            .find(|d| d.action == "move:INBOX")
            .unwrap()
            .reason,
        "bayes"
    );
}

#[tokio::test]
async fn a_failed_rescue_move_clears_its_keyword() {
    let mut env = Env::trained();
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    let uid = env.mail().deliver(
        "Junk",
        &authenticated("r1", "Alice <alice@friends.example>"),
        &[],
    );
    env.mail().fail("move", MailError::Server("nope".into()));

    env.engine.run_once().await.unwrap();
    assert_eq!(env.mail().flags("Junk", uid), Some(vec![]));
}

#[tokio::test]
async fn provider_junk_teaches_the_model_unless_tamis_is_already_sure() {
    let mut env = Env::trained_with(Mode::Live, |cfg| cfg.policy.rescue = false);
    env.store()
        .add_address("zed@nowhere.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver("Junk", &unknown("p1"), &[]);
    env.mail().deliver("Junk", &spam("p2"), &[]);
    env.mail().deliver("Junk", &unknown("p3"), &["$Junk"]);

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.trained, 1);
    assert_eq!(env.store().trained("<p1@x>").unwrap(), Some(Label::Spam));
    assert_eq!(
        env.store().trained("<p2@x>").unwrap(),
        None,
        "already sure: nothing to learn"
    );
    assert_eq!(
        env.store().trained("<p3@x>").unwrap(),
        None,
        "marked Junk by the user"
    );
    assert!(
        env.store().has_address("zed@nowhere.example").unwrap(),
        "the whitelist is left alone"
    );
    let decisions = env.decisions();
    assert_eq!(decisions.len(), 2, "p3 is the user's, not logged");
    assert!(decisions.iter().all(|d| d.reason == "provider junk"));
    let sure = decisions.iter().find(|d| d.message_id == "<p2@x>").unwrap();
    assert_eq!(sure.action, "none");
    assert!(sure.score >= 0.9);

    env.engine.run_once().await.unwrap();
    assert_eq!(env.decisions().len(), 2, "logged once");
}

#[tokio::test]
async fn junk_is_not_scanned_when_no_policy_wants_it() {
    let mut env = Env::with(Mode::Live, |cfg| {
        cfg.policy.learn_from_junk = false;
        cfg.policy.rescue = false;
        cfg.policy.learn_provider_junk = false;
    });

    env.engine.run_once().await.unwrap();

    assert!(!env.mail().log.contains(&"state Junk".to_owned()));
}

#[tokio::test]
async fn marks_that_contradict_tamis_are_learnt() {
    let cases = [
        ("Junk on good mail", good("k1"), "$Junk", Some(Label::Spam)),
        ("NotJunk on spam", spam("k1"), "$NotJunk", Some(Label::Good)),
        ("Apple Mail's Junk", good("k1"), "Junk", Some(Label::Spam)),
        ("a mark that agrees", spam("k1"), "$Junk", None),
    ];
    for (name, raw, mark, learnt) in cases {
        let mut env = Env::trained_with(Mode::Shadow, |_| {});
        env.start().await;
        let uid = env.mail().deliver("INBOX", &raw, &[]);
        env.engine.run_once().await.unwrap();
        env.mail().add_keyword("INBOX", &[uid], mark).await.unwrap();

        let summary = env.engine.run_once().await.unwrap();

        assert_eq!(env.store().trained("<k1@x>").unwrap(), learnt, "{name}");
        assert_eq!(summary.corrected, usize::from(learnt.is_some()), "{name}");
    }
}

#[tokio::test]
async fn ambiguous_and_unjudged_marks_are_ignored() {
    let mut env = Env::trained_with(Mode::Shadow, |_| {});
    env.start().await;
    let both = env.mail().deliver("INBOX", &good("k1"), &[]);
    env.engine.run_once().await.unwrap();
    env.mail()
        .add_keyword("INBOX", &[both], "$Junk")
        .await
        .unwrap();
    env.mail()
        .add_keyword("INBOX", &[both], "$NotJunk")
        .await
        .unwrap();
    env.mail()
        .deliver("INBOX", &good("never-judged"), &["$Junk"]);
    env.store()
        .set_cursor(
            "INBOX",
            crate::store::Cursor {
                uid_validity: 1,
                last_uid: 2,
            },
        )
        .unwrap();

    assert_eq!(env.engine.run_once().await.unwrap().corrected, 0);
}

#[tokio::test]
async fn with_condstore_unchanged_folders_are_not_searched_again() {
    let mut env = Env::trained_with(Mode::Shadow, |_| {});
    env.mail().condstore = true;
    env.start().await;
    let searches = |env: &mut Env| {
        env.mail()
            .log
            .iter()
            .filter(|c| *c == "search_keywords INBOX")
            .count()
    };
    let first = searches(&mut env);
    assert!(first > 0);

    env.engine.run_once().await.unwrap();
    assert_eq!(searches(&mut env), first, "nothing changed");

    let uid = env.mail().deliver("INBOX", &good("k1"), &[]);
    env.engine.run_once().await.unwrap();
    env.mail()
        .add_keyword("INBOX", &[uid], "$Junk")
        .await
        .unwrap();
    env.engine.run_once().await.unwrap();
    assert!(searches(&mut env) > first);
    assert_eq!(env.store().trained("<k1@x>").unwrap(), Some(Label::Spam));
}

#[tokio::test]
async fn keyword_learning_can_be_turned_off() {
    let mut env = Env::with(Mode::Live, |cfg| cfg.policy.learn_from_keywords = false);
    env.engine.run_once().await.unwrap();

    assert!(
        !env.mail()
            .log
            .iter()
            .any(|call| call.starts_with("search_keywords"))
    );
}
