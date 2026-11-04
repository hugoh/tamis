#![allow(
    clippy::unwrap_used,
    reason = "a test that cannot read its fixtures should panic"
)]

use std::fs;
use std::path::Path;

use base64::Engine as _;
use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;
use tamis::classify::{Label, Model};
use tamis::store::{
    AddressQuery, AddressSort, Cursor, Decision, DecisionQuery, DecisionSort, ModSeq, Store,
};

#[derive(Deserialize)]
struct Golden {
    decisions: Vec<GoldenDecision>,
    addresses: Vec<GoldenAddress>,
    passkeys: Vec<GoldenPasskey>,
    queries: Vec<GoldenQuery>,
    tokens: Vec<String>,
    counts: Vec<GoldenCounts>,
    total_spam: u64,
    total_good: u64,
    stats: std::collections::HashMap<String, i64>,
    cursor: GoldenCursor,
    mod_seq: GoldenModSeq,
    setting: String,
    trained: std::collections::HashMap<String, i64>,
    enrollment: String,
}

#[derive(Deserialize)]
#[allow(clippy::struct_excessive_bools, reason = "mirrors the journal columns")]
struct GoldenDecision {
    id: i64,
    at: i64,
    mailbox: String,
    uid: u32,
    message_id: String,
    from: String,
    subject: String,
    score: f64,
    verdict: String,
    reason: String,
    action: String,
    applied: bool,
    undone: bool,
    corrected_by: i64,
    undone_by: i64,
    corrects: Option<Vec<i64>>,
}

#[derive(Deserialize)]
struct GoldenAddress {
    address: String,
    source: String,
    added_at: i64,
}

#[derive(Deserialize)]
struct GoldenPasskey {
    id: i64,
    credential_id: String,
    name: String,
    credential: String,
    created_at: i64,
    last_used: i64,
}

#[derive(Deserialize)]
struct GoldenQuery {
    name: String,
    id: i64,
    verdict: String,
    search: String,
    sort: String,
    asc: bool,
    desc: bool,
    offset: i64,
    limit: i64,
    addresses: bool,
    want: Option<Vec<Value>>,
    total: i64,
}

#[derive(Deserialize)]
struct GoldenCounts {
    token: String,
    spam: u64,
    good: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct GoldenCursor {
    #[serde(rename = "UIDValidity")]
    uid_validity: u32,
    #[serde(rename = "LastUID")]
    last_uid: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct GoldenModSeq {
    #[serde(rename = "UIDValidity")]
    uid_validity: u32,
    highest: u64,
}

fn golden() -> Golden {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden/store.json");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// A copy, so the fixture itself is never opened for writing.
fn open_go_database() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("state.db");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden/state.db"),
        &copy,
    )
    .unwrap();
    let store = Store::open(&copy).unwrap();
    (dir, store)
}

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap()
}

#[test]
fn reads_a_database_the_go_version_wrote() {
    let (_dir, store) = open_go_database();
    let want = golden();

    let decisions = store.decisions(1000).unwrap();
    assert_eq!(decisions.len(), want.decisions.len());
    for (got, want) in decisions.iter().zip(&want.decisions) {
        let label = format!("decision {}", want.id);
        assert_eq!(got.id, want.id, "{label}");
        assert_eq!(got.at, ts(want.at), "{label}");
        assert_eq!(got.mailbox, want.mailbox, "{label}");
        assert_eq!(got.uid, want.uid, "{label}");
        assert_eq!(got.message_id, want.message_id, "{label}");
        assert_eq!(got.from, want.from, "{label}");
        assert_eq!(got.subject, want.subject, "{label}");
        assert!((got.score - want.score).abs() < f64::EPSILON, "{label}");
        assert_eq!(
            (
                got.verdict.as_str(),
                got.reason.as_str(),
                got.action.to_string()
            ),
            (
                want.verdict.as_str(),
                want.reason.as_str(),
                want.action.clone()
            ),
            "{label}"
        );
        assert_eq!(
            (got.applied, got.undone),
            (want.applied, want.undone),
            "{label}"
        );
        assert_eq!(
            (got.corrected_by, got.undone_by),
            (want.corrected_by, want.undone_by),
            "{label}"
        );
        assert_eq!(
            got.corrects,
            want.corrects.clone().unwrap_or_default(),
            "{label}"
        );
    }

    let addresses = store.addresses().unwrap();
    assert_eq!(addresses.len(), want.addresses.len());
    for (got, want) in addresses.iter().zip(&want.addresses) {
        assert_eq!(
            (&got.address, &got.source, got.added_at),
            (&want.address, &want.source, ts(want.added_at))
        );
    }

    let passkeys = store.passkeys().unwrap();
    assert_eq!(passkeys.len(), want.passkeys.len());
    for (got, want) in passkeys.iter().zip(&want.passkeys) {
        let credential = base64::engine::general_purpose::STANDARD
            .decode(&want.credential)
            .unwrap();
        assert_eq!(
            (got.id, &got.credential_id, &got.name),
            (want.id, &want.credential_id, &want.name)
        );
        assert_eq!(got.credential, credential);
        assert_eq!(got.created_at, ts(want.created_at));
        assert_eq!(
            got.last_used,
            (want.last_used != 0).then_some(ts(want.last_used))
        );
    }
}

#[test]
fn reads_the_model_and_bookkeeping() {
    let (_dir, store) = open_go_database();
    let want = golden();

    let model = store.model(&want.tokens).unwrap();
    assert_eq!(model.totals(), (want.total_spam, want.total_good));
    for counts in &want.counts {
        assert_eq!(
            model.counts(&counts.token),
            (counts.spam, counts.good),
            "{}",
            counts.token
        );
    }

    for (key, label) in &want.trained {
        let got = store.trained(key).unwrap().map(|l| match l {
            Label::Good => 0,
            Label::Spam => 1,
        });
        assert_eq!(got.unwrap_or(-1), *label, "{key}");
    }

    let stats = store.stats().unwrap();
    let got = [
        ("spam", stats.spam),
        ("good", stats.good),
        ("tokens", stats.tokens),
        ("addresses", stats.addresses),
        ("decisions", stats.decisions),
    ];
    for (name, value) in got {
        assert_eq!(value, want.stats[name], "stats {name}");
    }
    assert_eq!(
        stats.oldest_decision,
        Some(ts(want.stats["oldest_decision"]))
    );

    assert_eq!(
        store.cursor("INBOX").unwrap(),
        Some(Cursor {
            uid_validity: want.cursor.uid_validity,
            last_uid: want.cursor.last_uid
        })
    );
    assert_eq!(
        store.mod_seq("Junk").unwrap(),
        Some(ModSeq {
            uid_validity: want.mod_seq.uid_validity,
            highest: want.mod_seq.highest
        })
    );
    assert_eq!(
        store.setting("auth_server_id").unwrap().as_deref(),
        Some(want.setting.as_str())
    );
    assert_eq!(store.cursor("nope").unwrap(), None);

    assert!(
        store
            .consume_enrollment(&want.enrollment, ts(1_700_000_000))
            .unwrap()
    );
    assert!(
        !store
            .consume_enrollment(&want.enrollment, ts(1_700_000_000))
            .unwrap()
    );
}

#[test]
fn queries_match_the_go_store() {
    let (_dir, store) = open_go_database();

    for case in golden().queries {
        let want: Vec<Value> = case.want.clone().unwrap_or_default();

        let (got, total): (Vec<Value>, i64) = if case.addresses {
            let query = AddressQuery {
                search: case.search.clone(),
                sort: AddressSort::from_name(&case.sort),
                desc: case.desc,
                offset: case.offset,
                limit: case.limit,
            };
            let (found, total) = store.query_addresses(&query).unwrap();
            (
                found.into_iter().map(|a| Value::from(a.address)).collect(),
                total,
            )
        } else {
            let query = DecisionQuery {
                id: case.id,
                verdict: case.verdict.parse().ok(),
                search: case.search.clone(),
                sort: DecisionSort::from_name(&case.sort),
                asc: case.asc,
                offset: case.offset,
                limit: case.limit,
            };
            let (found, total) = store.query_decisions(&query).unwrap();
            (
                found
                    .into_iter()
                    .map(|d: Decision| Value::from(d.id))
                    .collect(),
                total,
            )
        };

        assert_eq!(got, want, "{}", case.name);
        assert_eq!(total, case.total, "{} total", case.name);
    }
}
