#![allow(
    clippy::unwrap_used,
    reason = "a test that cannot read its fixtures should panic"
)]

use std::fs;
use std::path::Path;

use serde::Deserialize;
use tamis::config::{Config, Mode};

#[derive(Deserialize)]
struct Case {
    file: String,
    error: Option<String>,
    config: Option<Summary>,
}

#[derive(Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the configuration flags"
)]
struct Summary {
    state: String,
    host: String,
    port: u16,
    addr: String,
    user: String,
    password_command: Option<Vec<String>>,
    auth_server_id: String,
    watch: Vec<String>,
    junk: String,
    sent: String,
    train_spam: String,
    train_good: String,
    rescue_to: String,
    mode: String,
    spam_threshold: f64,
    good_threshold: f64,
    poll_interval_nanos: u128,
    whitelist_requires_auth: bool,
    learn_from_junk: bool,
    learn_provider_junk: bool,
    learn_from_keywords: bool,
    rescue: bool,
    journal_retention_nanos: u128,
    rescue_threshold: f64,
    web_enabled: bool,
    web_listen: String,
    web_origin: String,
    web_tls_cert: String,
    web_tls_key: String,
    web_self_signed: bool,
    web_tls: bool,
    web_rpid: String,
    monitor_ping_url: String,
    monitor_quality_url: String,
    monitor_fail_after: u32,
    rules_script: String,
    rules_create_folders: bool,
}

fn env(key: &str) -> Option<String> {
    match key {
        "XDG_STATE_HOME" => Some("/xdg/state".to_owned()),
        "HOME" => Some("/home/test".to_owned()),
        _ => None,
    }
}

#[test]
fn matches_the_recorded_results() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let cases: Vec<Case> =
        serde_json::from_slice(&fs::read(root.join("golden/config.json")).unwrap()).unwrap();
    assert!(cases.len() > 10);

    for case in cases {
        let loaded = Config::load(&env, &root.join("config").join(&case.file));

        match (case.config, case.error) {
            (Some(want), None) => {
                let got = loaded.unwrap_or_else(|err| panic!("{}: {err}", case.file));
                check(&case.file, &got, &want);
            }
            (None, Some(want)) if want == "parse" => {
                assert!(loaded.is_err(), "{} should not parse", case.file);
            }
            (None, Some(want)) => {
                assert_eq!(
                    sorted_keys(&loaded.unwrap_err().to_string()),
                    sorted_keys(&want),
                    "{}",
                    case.file
                );
            }
            _ => panic!("{}: fixture has neither a config nor an error", case.file),
        }
    }
}

/// The unknown keys come out in the parser's own order, which differs.
fn sorted_keys(message: &str) -> String {
    let Some((head, keys)) = message.split_once("unknown keys ") else {
        return message.to_owned();
    };
    let mut keys: Vec<&str> = keys.split(", ").collect();
    keys.sort_unstable();
    format!("{head}{}", keys.join(", "))
}

fn check(file: &str, got: &Config, want: &Summary) {
    let mode = if got.policy.mode == Mode::Live {
        "live"
    } else {
        "shadow"
    };
    let nanos = |d: std::time::Duration| d.as_nanos();

    assert_eq!(got.state.to_string_lossy(), want.state, "{file}: state");
    assert_eq!(got.account.host, want.host, "{file}");
    assert_eq!(got.account.port, want.port, "{file}");
    assert_eq!(got.account.addr(), want.addr, "{file}");
    assert_eq!(got.account.user, want.user, "{file}");
    assert_eq!(
        got.account.password_command,
        want.password_command.clone().unwrap_or_default(),
        "{file}"
    );
    assert_eq!(got.account.auth_server_id, want.auth_server_id, "{file}");
    assert_eq!(got.folders.watch, want.watch, "{file}");
    assert_eq!(got.folders.junk, want.junk, "{file}");
    assert_eq!(got.folders.sent, want.sent, "{file}");
    assert_eq!(got.folders.train_spam, want.train_spam, "{file}");
    assert_eq!(got.folders.train_good, want.train_good, "{file}");
    assert_eq!(got.folders.rescue_to(), want.rescue_to, "{file}");
    assert_eq!(mode, want.mode, "{file}");
    assert!(
        (got.policy.spam_threshold - want.spam_threshold).abs() < f64::EPSILON,
        "{file}"
    );
    assert!(
        (got.policy.good_threshold - want.good_threshold).abs() < f64::EPSILON,
        "{file}"
    );
    assert_eq!(
        nanos(got.policy.poll_interval),
        want.poll_interval_nanos,
        "{file}"
    );
    assert_eq!(
        got.policy.whitelist_requires_auth, want.whitelist_requires_auth,
        "{file}"
    );
    assert_eq!(got.policy.learn_from_junk, want.learn_from_junk, "{file}");
    assert_eq!(
        got.policy.learn_provider_junk, want.learn_provider_junk,
        "{file}"
    );
    assert_eq!(
        got.policy.learn_from_keywords, want.learn_from_keywords,
        "{file}"
    );
    assert_eq!(got.policy.rescue, want.rescue, "{file}");
    assert_eq!(
        nanos(got.policy.journal_retention),
        want.journal_retention_nanos,
        "{file}"
    );
    assert!(
        (got.policy.rescue_threshold - want.rescue_threshold).abs() < f64::EPSILON,
        "{file}"
    );
    assert_eq!(got.web.enabled, want.web_enabled, "{file}");
    assert_eq!(got.web.listen, want.web_listen, "{file}");
    assert_eq!(got.web.origin, want.web_origin, "{file}");
    assert_eq!(got.web.tls_cert, want.web_tls_cert, "{file}");
    assert_eq!(got.web.tls_key, want.web_tls_key, "{file}");
    assert_eq!(got.web.self_signed, want.web_self_signed, "{file}");
    assert_eq!(got.web.tls(), want.web_tls, "{file}");
    assert_eq!(got.web.rpid(), want.web_rpid, "{file}");
    assert_eq!(got.monitor.ping_url, want.monitor_ping_url, "{file}");
    assert_eq!(got.monitor.quality_url, want.monitor_quality_url, "{file}");
    assert_eq!(got.monitor.fail_after, want.monitor_fail_after, "{file}");
    assert_eq!(got.rules.script, want.rules_script, "{file}");
    assert_eq!(
        got.rules.create_folders, want.rules_create_folders,
        "{file}"
    );
}
