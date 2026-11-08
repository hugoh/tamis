use super::*;

const RULES: &str =
    r#"require "fileinto"; if header :contains "list-id" "golang" { fileinto "Lists"; }"#;

fn listed() -> String {
    message(
        "l1",
        "Alice <alice@friends.example>",
        "Go news",
        "news",
        "List-Id: <golang-nuts.example>\r\nAuthentication-Results: mx.example; dmarc=pass header.from=friends.example\r\n",
    )
}

fn env_with_rules(mode: Mode, tweak: impl FnOnce(&mut Config)) -> Env {
    let mut env = Env::trained_with(mode, tweak);
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.engine
        .set_rules(Rules::parse(RULES.as_bytes()).unwrap());
    env.mail().add_folder("Lists");
    env
}

#[tokio::test]
async fn good_mail_is_filed_by_the_rules_in_live_mode() {
    let mut env = env_with_rules(Mode::Live, |_| {});
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().uids("INBOX"), Vec::<u32>::new());
    assert_eq!(
        env.mail().flags("Lists", 1),
        Some(vec!["$tamissorted".to_owned()])
    );
    let decision = &env.decisions()[0];
    assert_eq!(
        (
            decision.reason.as_str(),
            decision.action.to_string().as_str(),
            decision.applied
        ),
        ("rule", "move:Lists", true)
    );
}

const FLAG_RULES: &str = r#"require ["fileinto", "imap4flags"];
if header :contains "list-id" "golang" { setflag "\\Seen"; addflag "Golang"; fileinto "Lists"; }"#;

fn env_with_flag_rules(mode: Mode) -> Env {
    let mut env = Env::trained_with(mode, |_| {});
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.engine
        .set_rules(Rules::parse(FLAG_RULES.as_bytes()).unwrap());
    env.mail().add_folder("Lists");
    env
}

#[tokio::test]
async fn rule_flags_travel_with_the_filed_message() {
    let mut env = env_with_flag_rules(Mode::Live);
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);

    env.engine.run_once().await.unwrap();

    let mut flags = env.mail().flags("Lists", 1).unwrap();
    flags.sort();
    assert_eq!(flags, ["$tamissorted", "\\seen", "golang"]);
}

#[tokio::test]
async fn a_failed_move_clears_the_rule_flags_it_set() {
    let mut env = env_with_flag_rules(Mode::Live);
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);
    env.mail().fail("move", MailError::Server("refused".into()));

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().flags("INBOX", 1), Some(vec![]));
    assert!(env.store().pending_moves().unwrap().is_empty());
}

#[tokio::test]
async fn a_move_cut_off_by_a_lost_connection_clears_every_mark_next_pass() {
    let mut env = env_with_flag_rules(Mode::Live);
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);
    env.mail()
        .fail("move", MailError::Connection("gone".into()));
    env.mail()
        .fail("remove_keyword", MailError::Connection("gone".into()));
    assert!(env.engine.run_once().await.unwrap_err().is_connection());
    assert_eq!(env.store().pending_moves().unwrap()[0].keywords.len(), 3);

    env.engine.run_once().await.unwrap();

    assert!(env.store().pending_moves().unwrap().is_empty());
    let mut flags = env.mail().flags("Lists", 1).unwrap();
    flags.sort();
    assert_eq!(
        flags,
        ["$tamissorted", "\\seen", "golang"],
        "retried and filed"
    );
}

#[tokio::test]
async fn flags_alone_are_set_in_place_and_journalled() {
    let mut env = Env::trained_with(Mode::Live, |_| {});
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.engine.set_rules(
        Rules::parse(
            br#"require "imap4flags"; if header :contains "list-id" "golang" { addflag "\\Flagged"; }"#,
        )
        .unwrap(),
    );
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().uids("INBOX"), [1]);
    assert_eq!(
        env.mail().flags("INBOX", 1),
        Some(vec!["\\flagged".to_owned()])
    );
    let decision = &env.decisions()[0];
    assert_eq!(
        (
            decision.reason.as_str(),
            decision.action.to_string().as_str(),
            decision.applied
        ),
        ("rule", "keyword:\\Flagged", true)
    );
}

#[tokio::test]
async fn rule_flags_are_only_reported_in_shadow_mode() {
    let mut env = env_with_flag_rules(Mode::Shadow);
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().flags("INBOX", 1), Some(vec![]));
    assert!(!env.decisions()[0].applied);
}

#[tokio::test]
async fn the_rules_only_report_in_shadow_mode() {
    let mut env = env_with_rules(Mode::Shadow, |_| {});
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().uids("INBOX"), [1]);
    assert!(!env.decisions()[0].applied);
}

#[tokio::test]
async fn a_rule_naming_a_missing_folder_leaves_the_mail() {
    let mut env = env_with_rules(Mode::Live, |_| {});
    env.mail().remove_folder("Lists");
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().uids("INBOX"), [1], "the mail stays");
    assert_eq!(env.decisions()[0].action, "none");
}

#[tokio::test]
async fn a_rule_may_create_its_folder_when_allowed() {
    let mut env = env_with_rules(Mode::Live, |cfg| cfg.rules.create_folders = true);
    env.mail().remove_folder("Lists");
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);

    env.engine.run_once().await.unwrap();

    assert!(env.mail().exists("Lists").await.unwrap());
    assert_eq!(env.mail().uids("Lists"), [1]);
}

#[tokio::test]
async fn a_failed_rule_move_clears_the_sorted_keyword() {
    let mut env = env_with_rules(Mode::Live, |_| {});
    env.start().await;
    let uid = env.mail().deliver("INBOX", &listed(), &[]);
    env.mail().fail("move", MailError::Server("nope".into()));

    env.engine.run_once().await.unwrap();
    assert_eq!(env.mail().flags("INBOX", uid), Some(vec![]));
}

#[tokio::test]
async fn missing_rule_folders_are_reported_without_failing() {
    let mut env = env_with_rules(Mode::Live, |_| {});
    env.mail().remove_folder("Lists");
    env.engine.warn_missing_rule_folders().await.unwrap();

    let mut quiet = env_with_rules(Mode::Live, |cfg| cfg.rules.create_folders = true);
    quiet.engine.warn_missing_rule_folders().await.unwrap();
    assert!(
        !quiet
            .mail()
            .log
            .iter()
            .any(|call| call.starts_with("folders")),
        "nothing to check when folders are created"
    );
}
