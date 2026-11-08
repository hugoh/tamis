use super::*;

/// A live engine that has filed one spam message into Junk.

#[tokio::test]
async fn undoing_filed_spam_brings_it_back_as_good() {
    let (mut env, id) = filed_spam().await;

    let account = env.engine.undo(id).await.unwrap();

    assert_eq!(
        account,
        "moved back to INBOX, learnt as good, sender whitelisted"
    );
    assert!(env.mail().uids("Junk").is_empty());
    assert_eq!(
        env.mail().flags("INBOX", 2),
        Some(vec!["$tamistrained".to_owned()])
    );
    assert_eq!(env.store().trained("<s1@x>").unwrap(), Some(Label::Good));
    assert!(env.store().has_address("winner@lottery.example").unwrap());
    assert!(env.store().decision(id).unwrap().undone);
    let entry = &env.decisions()[0];
    assert_eq!(
        (
            entry.reason.as_str(),
            entry.action.to_string().as_str(),
            entry.verdict.as_str()
        ),
        ("undo", "move:INBOX", "good")
    );

    assert!(
        matches!(env.engine.undo(id).await.unwrap_err().to_string().as_str(), s if s.contains("already undone"))
    );
    assert_eq!(
        env.engine.run_once().await.unwrap().scanned,
        0,
        "it is not judged again in the inbox"
    );
}

#[tokio::test]
async fn undoing_a_rescue_sends_it_back_as_spam() {
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
    let id = decision_where(&env, |d| d.action == "move:INBOX");

    let account = env.engine.undo(id).await.unwrap();

    assert_eq!(
        account,
        "moved back to Junk, learnt as spam, sender removed from whitelist"
    );
    assert_eq!(env.mail().uids("Junk").len(), 1);
    assert_eq!(env.store().trained("<r1@x>").unwrap(), Some(Label::Spam));
    assert!(!env.store().has_address("alice@friends.example").unwrap());
    let undo = decision_where(&env, |d| d.reason == "undo");
    assert!(env.store().decision(undo).unwrap().score > 0.0);
}

#[tokio::test]
async fn undoing_an_uncertain_flag_clears_it() {
    let mut env = Env::trained();
    env.start().await;
    let uid = env.mail().deliver("INBOX", &unknown("u1"), &[]);
    env.engine.run_once().await.unwrap();
    let id = decision_where(&env, |d| d.action == "keyword:$TamisUncertain");

    assert_eq!(
        env.engine.undo(id).await.unwrap(),
        "cleared $TamisUncertain"
    );

    assert_eq!(env.mail().flags("INBOX", uid), Some(vec![]));
    assert_eq!(env.decisions()[0].action, "unflag:$TamisUncertain");
}

#[tokio::test]
async fn a_refused_move_to_junk_keeps_the_uncertain_flag_for_a_retry() {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("INBOX", &unknown("u1"), &[]);
    env.engine.run_once().await.unwrap();
    let id = decision_where(&env, |d| d.action == "keyword:$TamisUncertain");
    env.mail().fail("move", MailError::Server("nope".into()));

    assert!(env.engine.learn(id, Some(Label::Spam)).await.is_err());

    assert_eq!(
        env.mail().flags("INBOX", 1),
        Some(vec!["$tamisuncertain".to_owned()])
    );
    env.engine.learn(id, Some(Label::Spam)).await.unwrap();
    assert_eq!(env.mail().uids("Junk").len(), 1);
}

#[tokio::test]
async fn learning_an_uncertain_flag_as_spam_clears_it_and_moves_to_junk() {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("INBOX", &unknown("u1"), &[]);
    env.engine.run_once().await.unwrap();
    let id = decision_where(&env, |d| d.action == "keyword:$TamisUncertain");

    assert!(env.engine.learn(id, None).await.is_err());
    env.engine.learn(id, Some(Label::Spam)).await.unwrap();

    assert_eq!(env.mail().uids("Junk").len(), 1);
    assert!(env.mail().uids("INBOX").is_empty());
    assert_eq!(env.mail().flags("Junk", 1), Some(vec![]));
    assert_eq!(env.store().trained("<u1@x>").unwrap(), Some(Label::Spam));
}

#[tokio::test]
async fn learning_an_uncertain_flag_as_good_clears_it_in_place() {
    let mut env = Env::trained();
    env.start().await;
    let uid = env.mail().deliver("INBOX", &unknown("u1"), &[]);
    env.engine.run_once().await.unwrap();
    let id = decision_where(&env, |d| d.action == "keyword:$TamisUncertain");

    env.engine.learn(id, Some(Label::Good)).await.unwrap();

    assert_eq!(env.mail().flags("INBOX", uid), Some(vec![]));
    assert_eq!(env.store().trained("<u1@x>").unwrap(), Some(Label::Good));
}

#[tokio::test]
async fn some_decisions_cannot_be_undone() {
    let mut env = Env::trained_with(Mode::Shadow, |_| {});
    env.start().await;
    env.mail().deliver("INBOX", &spam("s1"), &[]);
    env.engine.run_once().await.unwrap();
    let recorded = decision_where(&env, |d| !d.applied);

    let training = {
        let message = Message {
            message_id: "<t@x>".into(),
            ..Message::default()
        };
        let judgement = Judgement {
            verdict: Verdict::Good,
            reason: Reason::TrainFolder,
            score: 0.0,
        };
        env.engine
            .record("INBOX", &message, &judgement, &Action::TrainGood, true)
            .unwrap();
        decision_where(&env, |d| d.reason == "train folder")
    };

    let cases = [
        (recorded, "nothing to undo"),
        (training, "only tamis's own filing"),
        (9999, "not found"),
    ];
    for (id, want) in cases {
        let err = env.engine.undo(id).await.unwrap_err().to_string();
        assert!(err.contains(want), "{id}: {err}");
    }
}

#[tokio::test]
async fn an_undo_that_cannot_finish_can_be_retried() {
    let (mut env, id) = filed_spam().await;

    env.mail().fail("move", MailError::Server("nope".into()));
    assert!(env.engine.undo(id).await.is_err());
    assert!(!env.store().decision(id).unwrap().undone);
    assert_eq!(
        env.mail().flags("Junk", 1),
        Some(vec!["$tamisfiled".to_owned()]),
        "the keyword it cleared is put back"
    );
    assert!(env.store().pending_moves().unwrap().is_empty());

    env.engine.undo(id).await.unwrap();
    assert!(env.store().decision(id).unwrap().undone);
}

#[tokio::test]
async fn an_undo_whose_reply_was_lost_is_settled_next_pass() {
    let (mut env, id) = filed_spam().await;
    env.mail()
        .fail_after("move", MailError::Connection("gone".into()));

    assert!(env.engine.undo(id).await.unwrap_err().is_connection());
    assert!(!env.store().decision(id).unwrap().undone);
    assert_eq!(env.mail().uids("INBOX"), [2], "the server did move it");

    env.engine.run_once().await.unwrap();

    assert!(env.store().decision(id).unwrap().undone);
    assert!(env.store().pending_moves().unwrap().is_empty());
    let undo = decision_where(&env, |d| d.reason == "undo");
    assert!(env.store().decision(undo).unwrap().applied);
}

#[tokio::test]
async fn an_undo_that_never_happened_puts_its_marks_back_next_pass() {
    let (mut env, id) = filed_spam().await;
    env.mail()
        .fail("move", MailError::Connection("gone".into()));

    assert!(env.engine.undo(id).await.unwrap_err().is_connection());
    assert_eq!(env.store().pending_moves().unwrap().len(), 1);
    // The connection was gone for the rollback too.
    env.mail()
        .remove_keyword("Junk", &[1], "$TamisTrained")
        .await
        .unwrap();
    env.mail()
        .remove_keyword("Junk", &[1], "$TamisFiled")
        .await
        .unwrap();

    env.engine.run_once().await.unwrap();

    assert!(env.store().pending_moves().unwrap().is_empty());
    assert!(!env.store().decision(id).unwrap().undone);
    assert_eq!(
        env.mail().flags("Junk", 1),
        Some(vec!["$tamisfiled".to_owned()])
    );
    assert_eq!(env.decisions().len(), 1, "no row left for the attempt");
}

#[tokio::test]
async fn undo_needs_the_message_where_tamis_left_it() {
    let (mut env, id) = filed_spam().await;
    env.mail()
        .move_messages("Junk", &[1], "TrainSpam")
        .await
        .unwrap();

    let err = env.engine.undo(id).await.unwrap_err();

    assert!(err.is_message_gone(), "{err}");
    assert!(!env.store().decision(id).unwrap().undone);
}
